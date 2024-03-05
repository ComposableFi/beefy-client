use super::ibc_provider::*;
use crate::{
	chain::{
		client_state_from_abi_token, consensus_state_from_abi_token, tm_header_from_abi_token,
	},
	client::{ClientError, EthereumClient},
	events::TryFromEvent,
	prove::prove_fast,
	utils::{create_intervals, SEQUENCES_PER_ITER},
};
use elliptic_curve::PrimeField;
use ethers::{
	abi,
	abi::{
		encode, encode_packed, ethabi, Abi, AbiEncode, Detokenize, InvalidOutputType, ParamType,
		RawLog, Token, Tokenizable,
	},
	contract::{abigen, EthEvent},
	middleware::contract::Contract,
	prelude::{Block, Log},
	providers::Middleware,
	types::{
		BlockId, BlockNumber, EIP1186ProofResponse, Filter, StorageProof, Topic, ValueOrArray,
		H160, H256, U256,
	},
	utils::keccak256,
};
use futures::{FutureExt, Stream, StreamExt};
use ibc::{
	applications::transfer::{Amount, BaseDenom, PrefixedCoin, PrefixedDenom, TracePath},
	core::{
		ics02_client::{
			client_state::ClientType, events::UpdateClient, msgs::update_client::MsgUpdateAnyClient,
		},
		ics04_channel::{
			channel::{Order, State},
			events::SendPacket,
			packet::Sequence,
		},
		ics23_commitment::commitment::{CommitmentPrefix, CommitmentRoot},
		ics24_host::{
			identifier::{ChannelId, ClientId, ConnectionId, PortId},
			path::{
				AcksPath, ChannelEndsPath, ClientConsensusStatePath, ClientStatePath,
				CommitmentsPath, ConnectionsPath, ReceiptsPath,
			},
			Path,
		},
	},
	events::IbcEvent,
	protobuf::Protobuf,
	timestamp::Timestamp,
	tx_msg::Msg,
	Height,
};
use ibc_proto::{
	google::protobuf::Any,
	ibc::core::{
		channel::v1::{
			Channel, Counterparty as ChannelCounterparty, IdentifiedChannel, QueryChannelResponse,
			QueryChannelsResponse, QueryNextSequenceReceiveResponse, QueryPacketCommitmentResponse,
			QueryPacketReceiptResponse,
		},
		client::v1::{QueryClientStateResponse, QueryConsensusStateResponse},
		commitment::v1::MerklePrefix,
		connection::v1::{
			ConnectionEnd, Counterparty as ConnectionCounterparty, IdentifiedConnection,
			QueryConnectionResponse, Version,
		},
	},
};
use ibc_rpc::{IbcApiClient, PacketInfo};
use ics07_tendermint::consensus_state::ConsensusState as TmConsensusState;
use icsxx_ethereum::{
	client_message::ClientMessage, client_state::ClientState, consensus_state::ConsensusState,
};
use itertools::Itertools;
use log::info;
use pallet_ibc::light_clients::{AnyClientMessage, AnyClientState, AnyConsensusState};
use primitives::{mock::LocalClientTypes, IbcProvider, KeyProvider, UpdateType};
use prost::Message;
use ssz_rs::{Merkleized, Node};
use std::{
	collections::{HashMap, HashSet},
	fs,
	future::Future,
	iter,
	pin::Pin,
	str::FromStr,
	time::Duration,
};
use sync_committee_primitives::{
	consensus_types::BeaconBlockHeader, types::VerifierState as LightClientState,
};
use tokio::time::sleep;

const EARLIEST_BLOCK: u64 = 0;

// #[cfg(feature = "no_indexer")]
impl EthereumClient {
	pub async fn query_client_state_exact_token(
		&self,
		at: Height,
		client_id: ClientId,
	) -> Result<Token, ClientError> {
		// First, we try to find an `UpdateClient` event at the given height...
		let mut client_state = None;
		let mut event_filter = self
			.yui
			.event_for_name::<UpdateClientFilter>("UpdateClient")
			.map_err(|err| {
				ClientError::Other(format!("contract is missing UpdateClient event: {}", err))
			})?
			.from_block(BlockNumber::Number(self.contract_creation_block().into()))
			.address(ValueOrArray::Value(self.yui.ibc_core_diamond.address()))
			.to_block(at.revision_height);
		event_filter.filter = event_filter.filter.topic1({
			let hash = H256::from_slice(&encode(&[Token::FixedBytes(
				keccak256(client_id.to_string().into_bytes()).to_vec(),
			)]));
			ValueOrArray::Value(hash)
		});
		let maybe_log = self
			.yui
			.ibc_core_diamond
			.client()
			.get_logs(&event_filter.filter)
			.await
			.map_err(
				|err| ClientError::Other(format!("failed to get logs 3: {}", err)),
			)?
			.pop() // get only the last event
		;
		let batch_func = self.yui.function("callBatch")?;
		match maybe_log {
			Some((_, log)) => {
				info!("found update client event at {}", log.block_number.unwrap());
				let tx_hash = log
					.transaction_hash
					.ok_or(ClientError::Other("tx hash not found".to_string()))?;
				let func = self.yui.function("updateClient")?;
				let tx = self
					.client()
					.get_transaction(tx_hash)
					.await
					.map_err(|err| {
						ClientError::Other(format!("failed to get transaction: {}", err))
					})?
					.ok_or_else(|| {
						ClientError::Other(format!("transaction not found: {}", tx_hash))
					})?;
				let Token::Array(batch_calldata) =
					batch_func
						.decode_input(&tx.input[4..])?
						.pop()
						.ok_or(ClientError::Other("batch calldata not found".to_string()))?
				else {
					return Err(ClientError::Other("batch calldata not found".to_string()))
				};

				for input_tok in batch_calldata.into_iter().rev() {
					let Token::Bytes(input) = input_tok else {
						return Err(ClientError::Other("input token should be bytes".to_string()))
					};
					if input[..4] == func.short_signature() {
						let calldata = func
							.decode_input(&input[4..])?
							.pop()
							.ok_or(ClientError::Other("calldata not found".to_string()))?;
						let Token::Tuple(toks) = calldata else {
							return Err(ClientError::Other("calldata should be bytes".to_string()))
						};
						let header = tm_header_from_abi_token(toks[1].clone())?;
						let client_state_token = toks[2].clone();
						// let mut cs =
						// 	client_state_from_abi_token::<LocalClientTypes>(client_state_token)?;
						// cs.latest_height = Height::new(
						// 	cs.latest_height.revision_number,
						// 	header.signed_header.header.height.into(),
						// );
						client_state = Some(client_state_token);
						// TODO: figure out how to distinguish between the same function calls
						break
					}
				}
				// TODO: handle frozen height
			},
			None => {
				log::trace!(target: "hyperspace_ethereum", "no update client event found for blocks ..{at}, looking for a create client event...");

				// ...otherwise, try to get the `CreateClient` event
				let mut event_filter = self
					.yui
					.event_for_name::<CreateClientFilter>("CreateClient")
					.map_err(|err| {
						ClientError::Other(format!(
							"contract is missing CreateClient event: {}",
							err
						))
					})?
					.from_block(BlockNumber::Number(self.contract_creation_block().into()))
					.address(ValueOrArray::Value(self.yui.ibc_core_diamond.address()))
					//		.from_block(self.contract_creation_block())
					.address(ValueOrArray::Value(self.yui.ibc_core_diamond.address()))
					.to_block(at.revision_height);
				event_filter.filter = event_filter.filter.topic1({
					let hash = H256::from_slice(&encode(&[Token::FixedBytes(
						keccak256(client_id.to_string().into_bytes()).to_vec(),
					)]));
					ValueOrArray::Value(hash)
				});
				let log = self
					.yui
					.ibc_core_diamond
					.client()
					.get_logs(&event_filter.filter)
					.await
					.map_err(|err| ClientError::Other(format!("failed to get logs 4: {}", err)))?
					.pop() // get only the last event
					.ok_or_else(|| ClientError::Other("no events found test4".to_string()))?;

				let tx_hash = log
					.transaction_hash
					.ok_or(ClientError::Other("tx hash not found".to_string()))?;
				let func = self.yui.function("createClient")?;
				let tx = self
					.client()
					.get_transaction(tx_hash)
					.await
					.map_err(|err| {
						ClientError::Other(format!("failed to get transaction: {}", err))
					})?
					.ok_or_else(|| {
						ClientError::Other(format!("transaction not found: {}", tx_hash))
					})?;

				let Token::Array(batch_calldata) =
					batch_func
						.decode_input(&tx.input[4..])?
						.pop()
						.ok_or(ClientError::Other("batch calldata not found".to_string()))?
				else {
					return Err(ClientError::Other("batch calldata not found".to_string()))
				};

				for input_tok in batch_calldata.into_iter().rev() {
					let Token::Bytes(input) = input_tok else {
						return Err(ClientError::Other("input token should be bytes".to_string()))
					};
					if input[..4] == func.short_signature() {
						let calldata = func
							.decode_input(&input[4..])?
							.pop()
							.ok_or(ClientError::Other("calldata not found".to_string()))?;
						let Token::Tuple(toks) = calldata else {
							return Err(ClientError::Other("calldata should be bytes".to_string()))
						};
						let client_state_token = toks[1].clone();

						client_state = Some(client_state_token);

						break
					}
				}
			},
		}

		Ok(client_state.ok_or(ClientError::Other("client state not found".to_string()))?)
	}
}

#[async_trait::async_trait]
impl IbcProvider for EthereumClient {
	type FinalityEvent = Block<H256>;

	type TransactionId = (H256, H256);

	type AssetId = String;

	type Error = ClientError;

	async fn query_latest_ibc_events<T>(
		&mut self,
		finality_event: Self::FinalityEvent,
		counterparty: &T,
	) -> Result<Vec<(Any, Height, Vec<IbcEvent>, UpdateType)>, anyhow::Error>
	where
		T: primitives::Chain,
	{
		let client_id = self.client_id();
		let latest_cp_height = counterparty.latest_height_and_timestamp().await?.0;
		let latest_cp_client_state =
			counterparty.query_client_state(latest_cp_height, client_id.clone()).await?;
		let client_state_response = latest_cp_client_state.client_state.ok_or_else(|| {
			ClientError::Other("counterparty returned empty client state".to_string())
		})?;
		let AnyClientState::Ethereum(client_state) =
			AnyClientState::decode_recursive(client_state_response, |c| {
				matches!(c, AnyClientState::Ethereum(_))
			})
			.ok_or_else(|| ClientError::Other(format!("Could not decode client state")))?
		else {
			unreachable!()
		};
		let latest_cp_client_height = client_state.latest_height().revision_height;
		let latest_height = self.latest_height_and_timestamp().await?.0;
		let latest_revision = latest_height.revision_number;

		let prover = self.prover();
		let block = prover.fetch_block("head").await?;
		let block_to = (client_state.inner.finalized_header.slot +
			NUMBER_OF_BLOCKS_TO_PROCESS_PER_ITER)
			.min(block.slot);

		let number = block.body.execution_payload.block_number;

		let from = latest_cp_client_height + 1;
		let to = number.min(latest_cp_client_height + NUMBER_OF_BLOCKS_TO_PROCESS_PER_ITER / 2);

		log::info!(target: "hyperspace_ethereum", "Getting blocks {}..{}", from, to);
		let filter = Filter::new()
			.from_block(from)
			.to_block(to)
			.address(self.yui.ibc_core_diamond.address());
		let mut logs = self
			.client()
			.get_logs(&filter)
			.await
			.map_err(|e| ClientError::Other(format!("failed to get logs 1: {}", e)))?;
		let filter = Filter::new().from_block(from).to_block(to).address(
			self.yui
				.bank
				.as_ref()
				.ok_or(ClientError::Other("bank contract not found".to_string()))?
				.address(),
		);
		let logs2 = self
			.client()
			.get_logs(&filter)
			.await
			.map_err(|e| ClientError::Other(format!("failed to get logs 2: {}", e)))?;
		logs.extend(logs2);

		let tries = NUMBER_OF_BLOCKS_TO_PROCESS_PER_ITER / 2;
		let mut i = 0;
		let header = loop {
			let maybe_proof = prove_fast(self, &client_state, block_to + i).await;
			i += 1;
			match maybe_proof {
				Ok(x) => break x,
				Err(e) => {
					log::error!(target: "hyperspace_ethereum", "failed to prove {e}");
					std::thread::sleep(std::time::Duration::from_secs(2));
					if i >= tries {
						return Ok(vec![])
					}
				},
			};
		};
		let update = &header.inner;
		// let update = prove(self, finality_event.number.unwrap().as_u64()).await?;

		log::info!(target: "hyperspace_ethereum",
			"proven: state root = {}, body root = {}, slot = {}, block number = {}",
			update.finalized_header.state_root,
			update.finalized_header.body_root,
			update.finalized_header.slot,
			update.execution_payload.block_number
		);
		// finality_checkpoint.finalized.epoch <= client_state.latest_finalized_epoch
		if update.execution_payload.block_number <= client_state.latest_height().revision_height {
			log::info!(target: "hyperspace_ethereum", "no new events");
			return Ok(vec![])
		}
		let update_height =
			Height::new(latest_revision, update.execution_payload.block_number.into());
		// let update_height = Height::new(latest_revision, update.finalized_header.slot.into());
		let events = parse_ethereum_events(&self, logs).await?;

		let update_client_header = {
			log::info!(target: "hyperspace_ethereum", "update client header height: {}, finalized slot: {}",
				update.execution_payload.block_number,
				update.finalized_header.slot
			);
			let msg = MsgUpdateAnyClient::<LocalClientTypes> {
				client_id: client_id.clone(),
				client_message: AnyClientMessage::Ethereum(ClientMessage::Header(header)),
				signer: counterparty.account_id(),
			};
			let value = msg.encode_vec().map_err(|e| {
				ClientError::from(format!("Failed to encode MsgUpdateClient {msg:?}: {e:?}"))
			})?;
			Any { value, type_url: msg.type_url() }
		};

		Ok(vec![(update_client_header, update_height, events, UpdateType::Mandatory)])
	}

	// TODO: this function is mostly used in tests and in 'fishing' mode.
	async fn ibc_events(&self) -> Pin<Box<dyn Stream<Item = IbcEvent> + Send + 'static>> {
		let ibc_address = self.yui.ibc_core_diamond.address();
		let client = self.clone();

		let ws = self.websocket_provider().await.unwrap();
		(async_stream::stream! {
			let mut events_stream = ws.subscribe_logs(
				 &Filter::new()
				 .from_block(BlockNumber::Number(EARLIEST_BLOCK.into()))
				 //.from_block(BlockNumber::Earliest)
				 .address(ibc_address),
			)
			.await
			.unwrap()
			.filter_map(|log| async {
				let raw_log = RawLog::from(log.clone());
				let height = Height::new(0, log.block_number.unwrap().as_u64());
				let topic0 = log.topics[0];

				let mut maybe_ibc_event = if topic0 == UpdateClientHeightFilter::signature() {
					let event = UpdateClientHeightFilter::decode_log(&raw_log).expect("decode event");
					 let topic1 = H256::from_slice(&encode(&[Token::FixedBytes(
						 keccak256("07-tendermint-0".to_string().into_bytes()).to_vec(),
					 )]));
				} else {
					log::warn!(target: "hyperspace_ethereum",
						"unknown event: {}",
						log.log_type.unwrap_or(format!("{topic0:?}"))
					);
				};

				Some(IbcEvent::Empty("".into()))
			}).boxed();

			while let Some(ev) = events_stream.next().await {
				yield ev
			}
		})
		.boxed()
	}

	async fn query_client_consensus(
		&self,
		at: Height,
		client_id: ClientId,
		consensus_height: Height,
	) -> Result<QueryConsensusStateResponse, Self::Error> {
		log::info!(target: "hyperspace_ethereum", "query_client_consensus: {client_id:?}, {consensus_height:?}");

		/*
		let binding = self
			.yui
			.method(
				"getConsensusState",
				(
					Token::String(client_id.as_str().to_owned()),
					Token::Tuple(vec![
						Token::Uint(consensus_height.revision_number.into()),
						Token::Uint(consensus_height.revision_height.into()),
					]),
				),
			)
			.map_err(
				|err| ClientError::Other(format!("contract is missing getConsensusState {}", err)),
			)?;

		let (client_cons, _): (Vec<u8>, bool) = binding
			.block(BlockId::Number(BlockNumber::Number(at.revision_height.into())))
			.call()
			.await
			.map_err(|err| {
				log::error!(target: "hyperspace_ethereum", "error: {err}");
				err
			})
			.map_err(|err| ClientError::Other(format!("failed to query client consensus: {}", err)))?;

		let proof_height = Some(at.into());
		let mut cs = client_state_from_abi_token::<LocalClientTypes>(client_state_token)?;
		 */

		// First, we try to find an `UpdateClient` event at the given height...
		let mut consensus_state = None;
		let mut event_filter = self
			.yui
			.event_for_name::<UpdateClientHeightFilter>("UpdateClientHeight")
			.expect("contract is missing UpdateClient event")
			.to_block(at.revision_height)
			.from_block(at.revision_height);
		event_filter.filter = event_filter
			.filter
			.topic1({
				let hash = H256::from_slice(&encode(&[Token::FixedBytes(
					keccak256(client_id.to_string().into_bytes()).to_vec(),
				)]));
				ValueOrArray::Value(hash)
			})
			.topic2({
				let height_bytes = encode(&[Token::Tuple(vec![
					Token::Uint(consensus_height.revision_number.into()),
					Token::Uint(consensus_height.revision_height.into()),
				])]);
				ValueOrArray::Value(H256::from_slice(&encode(&[Token::FixedBytes(
					keccak256(&height_bytes).to_vec(),
				)])))
			});
		let maybe_log = self
			.yui
			.ibc_core_diamond
			.client()
			.get_logs(&event_filter.filter)
			.await
			.unwrap()
			.pop() // get only the last event
			;
		let batch_func = self.yui.function("callBatch")?;
		match maybe_log {
			Some(log) => {
				let tx_hash = log.transaction_hash.expect("tx hash should exist");
				let func = self.yui.function("updateClient")?;
				let tx =
					self.client().get_transaction(tx_hash).await.unwrap().ok_or_else(|| {
						ClientError::Other(format!("transaction not found: {}", tx_hash))
					})?;
				let Token::Array(batch_calldata) =
					batch_func.decode_input(&tx.input[4..])?.pop().unwrap()
				else {
					return Err(ClientError::Other("batch calldata not found".to_string()))
				};

				for input_tok in batch_calldata.into_iter().rev() {
					let Token::Bytes(input) = input_tok else { panic!() };
					if input[..4] == func.short_signature() {
						let calldata = func.decode_input(&input[4..])?.pop().unwrap();
						let Token::Tuple(toks) = calldata else { panic!() };
						let header = tm_header_from_abi_token(toks[1].clone())?;
						consensus_state = Some(TmConsensusState {
							timestamp: header.signed_header.header.time,
							root: CommitmentRoot {
								bytes: header.signed_header.header.app_hash.as_bytes().to_owned(),
							},
							next_validators_hash: header.signed_header.header.next_validators_hash,
						});
						// TODO: figure out how to distinguish between the same function calls

						let proof_height = Some(at.into());
						return Ok(QueryConsensusStateResponse {
							consensus_state: Some(
								consensus_state.expect("should always be initialized").to_any(),
							),
							proof: vec![0],
							proof_height,
						})
					}
				}
				// TODO: handle frozen height
			},
			None => {},
		}

		log::trace!(target: "hyperspace_ethereum", "no update client event found for blocks ..{at}, looking for a create client event...");

		// ...otherwise, try to get the `CreateClient` event
		let mut event_filter = self
			.yui
			.event_for_name::<CreateClientFilter>("CreateClient")
			.expect("contract is missing CreateClient event")
			.from_block(BlockNumber::Number(EARLIEST_BLOCK.into()))
			.address(ValueOrArray::Value(self.yui.ibc_core_diamond.address()))
			//            .from_block(BlockNumber::Earliest)
			.to_block(at.revision_height);
		event_filter.filter = event_filter.filter.topic1({
			let hash = H256::from_slice(&encode(&[Token::FixedBytes(
				keccak256(client_id.to_string().into_bytes()).to_vec(),
			)]));
			ValueOrArray::Value(hash)
		});
		let log = self
			.yui
			.ibc_core_diamond
			.client()
			.get_logs(&event_filter.filter)
			.await
			.unwrap()
			.pop() // get only the last event
			.ok_or_else(|| ClientError::Other("no events found test5".to_string()))?;

		let tx_hash = log.transaction_hash.expect("tx hash should exist");
		let func = self.yui.function("createClient")?;
		let tx = self
			.client()
			.get_transaction(tx_hash)
			.await
			.unwrap()
			.ok_or_else(|| ClientError::Other(format!("transaction not found: {}", tx_hash)))?;

		let Token::Array(batch_calldata) = batch_func.decode_input(&tx.input[4..])?.pop().unwrap()
		else {
			return Err(ClientError::Other("batch calldata not found".to_string()))
		};

		for input_tok in batch_calldata.into_iter().rev() {
			let Token::Bytes(input) = input_tok else { panic!() };
			log::info!("sig = {:?}", func.short_signature());
			if input[..4] == func.short_signature() {
				let calldata = func.decode_input(&input[4..])?.pop().unwrap();
				let Token::Tuple(toks) = calldata else { panic!() };
				let consensus_state_token = toks[2].clone();
				consensus_state = Some(consensus_state_from_abi_token(consensus_state_token)?);
				break
			}
		}

		let proof_height = Some(at.into());
		let any = consensus_state.expect("should always be initialized").to_any();

		Ok(QueryConsensusStateResponse { consensus_state: Some(any), proof: vec![0], proof_height })
	}

	async fn query_client_state(
		&self,
		at: Height,
		client_id: ClientId,
	) -> Result<QueryClientStateResponse, Self::Error> {
		// First, we try to find an `UpdateClient` event at the given height...
		let mut client_state = None;
		let mut event_filter = self
			.yui
			.event_for_name::<UpdateClientFilter>("UpdateClient")
			.map_err(|err| {
				ClientError::Other(format!("contract is missing UpdateClient event: {}", err))
			})?
			.from_block(BlockNumber::Number(EARLIEST_BLOCK.into()))
			.address(ValueOrArray::Value(self.yui.ibc_core_diamond.address()))
			.to_block(at.revision_height);
		event_filter.filter = event_filter.filter.topic1({
			let hash = H256::from_slice(&encode(&[Token::FixedBytes(
				keccak256(client_id.to_string().into_bytes()).to_vec(),
			)]));
			ValueOrArray::Value(hash)
		});
		let maybe_log = self
			.yui
			.ibc_core_diamond
			.client()
			.get_logs(&event_filter.filter)
			.await
			.map_err(
				|err| ClientError::Other(format!("failed to get logs 3: {}", err)),
			)?
			.pop() // get only the last event
			;
		let batch_func = self.yui.function("callBatch")?;
		match maybe_log {
			Some(log) => {
				let tx_hash = log
					.transaction_hash
					.ok_or(ClientError::Other("tx hash not found".to_string()))?;
				let func = self.yui.function("updateClient")?;
				let tx = self
					.client()
					.get_transaction(tx_hash)
					.await
					.map_err(|err| {
						ClientError::Other(format!("failed to get transaction: {}", err))
					})?
					.ok_or_else(|| {
						ClientError::Other(format!("transaction not found: {}", tx_hash))
					})?;
				let Token::Array(batch_calldata) =
					batch_func
						.decode_input(&tx.input[4..])?
						.pop()
						.ok_or(ClientError::Other("batch calldata not found".to_string()))?
				else {
					return Err(ClientError::Other("batch calldata not found".to_string()))
				};

				for input_tok in batch_calldata.into_iter().rev() {
					let Token::Bytes(input) = input_tok else {
						return Err(ClientError::Other("input token should be bytes".to_string()))
					};
					if input[..4] == func.short_signature() {
						let calldata = func
							.decode_input(&input[4..])?
							.pop()
							.ok_or(ClientError::Other("calldata not found".to_string()))?;
						let Token::Tuple(toks) = calldata else {
							return Err(ClientError::Other("calldata should be bytes".to_string()))
						};
						let header = tm_header_from_abi_token(toks[1].clone())?;
						let client_state_token = toks[2].clone();
						let mut cs =
							client_state_from_abi_token::<LocalClientTypes>(client_state_token)?;
						cs.latest_height = Height::new(
							cs.latest_height.revision_number,
							header.signed_header.header.height.into(),
						);
						client_state = Some(cs);
						// TODO: figure out how to distinguish between the same function calls
						break
					}
				}
				// TODO: handle frozen height
			},
			None => {
				log::trace!(target: "hyperspace_ethereum", "no update client event found for blocks ..{at}, looking for a create client event...");

				// ...otherwise, try to get the `CreateClient` event
				let mut event_filter = self
					.yui
					.event_for_name::<CreateClientFilter>("CreateClient")
					.map_err(|err| {
						ClientError::Other(format!(
							"contract is missing CreateClient event: {}",
							err
						))
					})?
					.from_block(BlockNumber::Number(EARLIEST_BLOCK.into()))
					.address(ValueOrArray::Value(self.yui.ibc_core_diamond.address()))
					//		.from_block(BlockNumber::Earliest)
					.to_block(at.revision_height);
				event_filter.filter = event_filter.filter.topic1({
					let hash = H256::from_slice(&encode(&[Token::FixedBytes(
						keccak256(client_id.to_string().into_bytes()).to_vec(),
					)]));
					ValueOrArray::Value(hash)
				});
				let log = self
					.yui
					.ibc_core_diamond
					.client()
					.get_logs(&event_filter.filter)
					.await
					.map_err(|err| ClientError::Other(format!("failed to get logs 4: {}", err)))?
					.pop() // get only the last event
					.ok_or_else(|| ClientError::Other("no events found test6".to_string()))?;

				let tx_hash = log
					.transaction_hash
					.ok_or(ClientError::Other("tx hash not found".to_string()))?;
				let func = self.yui.function("createClient")?;
				let tx = self
					.client()
					.get_transaction(tx_hash)
					.await
					.map_err(|err| {
						ClientError::Other(format!("failed to get transaction: {}", err))
					})?
					.ok_or_else(|| {
						ClientError::Other(format!("transaction not found: {}", tx_hash))
					})?;

				let Token::Array(batch_calldata) =
					batch_func
						.decode_input(&tx.input[4..])?
						.pop()
						.ok_or(ClientError::Other("batch calldata not found".to_string()))?
				else {
					return Err(ClientError::Other("batch calldata not found".to_string()))
				};

				for input_tok in batch_calldata.into_iter().rev() {
					let Token::Bytes(input) = input_tok else {
						return Err(ClientError::Other("input token should be bytes".to_string()))
					};
					if input[..4] == func.short_signature() {
						let calldata = func
							.decode_input(&input[4..])?
							.pop()
							.ok_or(ClientError::Other("calldata not found".to_string()))?;
						let Token::Tuple(toks) = calldata else {
							return Err(ClientError::Other("calldata should be bytes".to_string()))
						};
						let client_state_token = toks[1].clone();
						client_state = Some(client_state_from_abi_token::<LocalClientTypes>(
							client_state_token,
						)?);
						break
					}
				}
			},
		}

		let proof_height = Some(at.into());

		Ok(QueryClientStateResponse {
			client_state: Some(
				client_state
					.ok_or(ClientError::Other("client state not found".to_string()))?
					.to_any(),
			),
			proof_height,
			proof: vec![0],
		})
	}

	async fn query_connection_end(
		&self,
		at: Height,
		connection_id: ConnectionId,
	) -> Result<QueryConnectionResponse, Self::Error> {
		let (connection_end, exists): (ConnectionEndData, bool) = self
			.yui
			.method("getConnection", (connection_id.to_string(),))
			.map_err(|err| {
				ClientError::Other(format!("contract is missing getConnectionEnd {}", err))
			})?
			.block(BlockId::Number(BlockNumber::Number(at.revision_height.into())))
			.call()
			.await
			.map_err(|err| ClientError::Other(format!("failed to query connection: {}", err)))?;

		let connection = if exists {
			let prefix = if connection_end.counterparty.prefix.key_prefix.0.is_empty() {
				None
			} else {
				Some(MerklePrefix {
					key_prefix: connection_end.counterparty.prefix.key_prefix.to_vec(),
				})
			};

			Some(ConnectionEnd {
				client_id: connection_end.client_id,
				versions: connection_end
					.versions
					.into_iter()
					.map(|v| Version { identifier: v.identifier, features: v.features })
					.collect(),
				state: connection_end.state as _,
				counterparty: Some(ConnectionCounterparty {
					client_id: connection_end.counterparty.client_id,
					connection_id: connection_end.counterparty.connection_id,
					prefix,
				}),
				delay_period: connection_end.delay_period,
			})
		} else {
			None
		};

		Ok(QueryConnectionResponse { connection, proof: vec![0], proof_height: Some(at.into()) })
	}

	async fn query_channel_end(
		&self,
		at: Height,
		channel_id: ChannelId,
		port_id: PortId,
	) -> Result<QueryChannelResponse, Self::Error> {
		let binding = self
			.yui
			.method::<_, ChannelData>(
				"getChannel",
				(port_id.as_str().to_owned(), channel_id.to_string()),
			)
			.map_err(|err| ClientError::Other(format!("contract is missing getChannel {}", err)))?;

		let channel_data = binding
			.block(BlockId::Number(BlockNumber::Number(at.revision_height.into())))
			.call()
			.await
			.map_err(|err| ClientError::Other(format!("failed to query channel: {}", err)))?;

		let _state = State::from_i32(channel_data.state as _)
			.map_err(|err| ClientError::Other(format!("invalid channel state: {}", err)))?;
		let counterparty = Some(ChannelCounterparty {
			port_id: channel_data.counterparty.port_id,
			channel_id: channel_data.counterparty.channel_id,
		});
		Ok(QueryChannelResponse {
			channel: Some(Channel {
				state: channel_data.state as _,
				ordering: channel_data.ordering as _,
				counterparty,
				connection_hops: channel_data.connection_hops,
				version: channel_data.version,
			}),
			proof: vec![0],
			proof_height: Some(at.into()),
		})
	}

	async fn query_proof(&self, at: Height, keys: Vec<Vec<u8>>) -> Result<Vec<u8>, Self::Error> {
		assert_eq!(keys.len(), 1);
		let key = String::from_utf8(keys[0].clone()).unwrap();
		let (bytes, _) = self.query_proof_with_value(&key, at).await?;
		Ok(bytes)
	}

	async fn query_packet_commitment(
		&self,
		at: Height,
		port_id: &PortId,
		channel_id: &ChannelId,
		seq: u64,
	) -> Result<QueryPacketCommitmentResponse, Self::Error> {
		let path = Path::Commitments(CommitmentsPath {
			port_id: port_id.clone(),
			channel_id: channel_id.clone(),
			sequence: Sequence::from(seq),
		})
		.to_string();

		let proof = self
			.eth_query_proof(&path, Some(at.revision_height), COMMITMENTS_STORAGE_INDEX)
			.await?;
		let storage = proof
			.storage_proof
			.first()
			.ok_or(ClientError::Other("storage proof not found".to_string()))?;
		let bytes = u256_to_bytes(&storage.value);

		Ok(QueryPacketCommitmentResponse {
			commitment: bytes,
			proof: encode(&[Token::Array(
				storage.proof.clone().into_iter().map(|p| Token::Bytes(p.to_vec())).collect(),
			)]),
			proof_height: Some(at.into()),
		})
	}

	async fn query_packet_acknowledgement(
		&self,
		at: Height,
		port_id: &PortId,
		channel_id: &ChannelId,
		seq: u64,
	) -> Result<ibc_proto::ibc::core::channel::v1::QueryPacketAcknowledgementResponse, Self::Error>
	{
		let path = Path::Acks(AcksPath {
			port_id: port_id.clone(),
			channel_id: channel_id.clone(),
			sequence: Sequence::from(seq),
		})
		.to_string();

		let proof = self
			.eth_query_proof(&path, Some(at.revision_height), COMMITMENTS_STORAGE_INDEX)
			.await?;
		let storage = proof
			.storage_proof
			.first()
			.ok_or(ClientError::Other("storage proof not found".to_string()))?;

		let bytes = u256_to_bytes(&storage.value);

		Ok(ibc_proto::ibc::core::channel::v1::QueryPacketAcknowledgementResponse {
			acknowledgement: bytes,
			proof: encode(&[Token::Array(
				storage.proof.clone().into_iter().map(|p| Token::Bytes(p.to_vec())).collect(),
			)]),
			proof_height: Some(at.into()),
		})
	}

	async fn query_next_sequence_recv(
		&self,
		at: Height,
		port_id: &PortId,
		channel_id: &ChannelId,
	) -> Result<QueryNextSequenceReceiveResponse, Self::Error> {
		let binding = self
			.yui
			.method::<_, u64>(
				"getNextSequenceRecv",
				(port_id.as_str().to_owned(), channel_id.to_string()),
			)
			.map_err(|err| {
				ClientError::Other(format!("contract is missing getNextSequenceRecv {}", err))
			})?;

		let seq = binding
			.block(BlockId::Number(BlockNumber::Number(at.revision_height.into())))
			.call()
			.await
			.map_err(|err| ClientError::Other(format!("failed to query channel_data: {}", err)))?;
		Ok(QueryNextSequenceReceiveResponse {
			next_sequence_receive: seq,
			proof: vec![], // TODO: implement proof for query_next_sequence_recv
			proof_height: None,
		})
	}

	async fn query_packet_receipt(
		&self,
		at: Height,
		port_id: &PortId,
		channel_id: &ChannelId,
		sequence: u64,
	) -> Result<QueryPacketReceiptResponse, Self::Error> {
		let path = Path::Receipts(ReceiptsPath {
			port_id: port_id.clone(),
			channel_id: channel_id.clone(),
			sequence: Sequence::from(sequence),
		})
		.to_string();

		let proof = self
			.eth_query_proof(&path, Some(at.revision_height), COMMITMENTS_STORAGE_INDEX)
			.await?;
		let storage = proof
			.storage_proof
			.first()
			.ok_or(ClientError::Other("storage proof not found".to_string()))?;

		let received = self
			.has_packet_receipt(at, port_id.as_str().to_owned(), format!("{channel_id}"), sequence)
			.await?;

		Ok(QueryPacketReceiptResponse {
			received,
			proof: encode(&[Token::Array(
				storage.proof.clone().into_iter().map(|p| Token::Bytes(p.to_vec())).collect(),
			)]),
			proof_height: Some(at.into()),
		})
	}

	async fn latest_height_and_timestamp(&self) -> Result<(Height, Timestamp), Self::Error> {
		// TODO: fix latest_height_and_timestamp in basic builds
		let prover = self.prover();
		let block = prover.fetch_block("head").await?;
		let number = block.body.execution_payload.block_number;
		let height = Height::new(0, number.into());
		let block = self
			.client()
			.get_block(BlockId::Number(number.into()))
			.await
			.map_err(|err| ClientError::MiddlewareError(err))?
			.ok_or_else(|| ClientError::Other("block not found".to_string()))?;

		let nanoseconds = Duration::from_secs(block.timestamp.as_u64()).as_nanos() as u64;
		let timestamp = Timestamp::from_nanoseconds(nanoseconds).map_err(|e| {
			ClientError::Other(format!("failed to parse timestamp : {nanoseconds}, err: {e}"))
		})?;

		log::info!(target: "hyperspace_ethereum", "latest_height_and_timestamp: {height:?}, {timestamp:?}");

		Ok((height, timestamp))
	}

	async fn query_packet_commitments(
		&self,
		at: Height,
		channel_id: ChannelId,
		port_id: PortId,
	) -> Result<Vec<u64>, Self::Error> {
		let start_seq = 0u64;
		let end_seq = 255u64;
		let binding = self
			.yui
			.method(
				"hasCommitments",
				(port_id.as_str().to_owned(), channel_id.to_string(), start_seq, end_seq),
			)
			.map_err(|err| {
				ClientError::Other(format!("contract is missing hasCommitments {}", err))
			})?;

		let bitmap: U256 = binding
			.block(BlockId::Number(BlockNumber::Number(at.revision_height.into())))
			.call()
			.await
			.map_err(|err| {
				ClientError::Other(format!("failed to query_packet_commitments: {}", err))
			})?;
		let mut seqs = vec![];
		for i in 0..256u64 {
			if bitmap.bit(i as _).into() {
				println!("bit {} is set", i);
				seqs.push(start_seq + i);
			}
		}

		// next_ack is the sequence number used when acknowledging packets.
		// the value of next_ack is the sequence number of the next packet to be acknowledged yet.
		// aka the last acknowledged packet was next_ack - 1.

		// this function is called to calculate which acknowledgements have not yet been
		// relayed from this chain to the counterparty chain.
		Ok(seqs)
	}

	async fn query_packet_acknowledgements(
		&self,
		at: Height,
		channel_id: ChannelId,
		port_id: PortId,
	) -> Result<Vec<u64>, Self::Error> {
		let start_seq = 0u64;
		let end_seq = 255u64;
		let binding = self
			.yui
			.method(
				"hasAcknowledgements",
				(port_id.as_str().to_owned(), channel_id.to_string(), start_seq, end_seq),
			)
			.map_err(|err| {
				ClientError::Other(format!("contract is missing hasAcknowledgements {}", err))
			})?;

		let bitmap: U256 = binding
			.block(BlockId::Number(BlockNumber::Number(at.revision_height.into())))
			.call()
			.await
			.map_err(|err| {
				ClientError::Other(format!("failed to query_packet_acknowledgements: {}", err))
			})?;
		let mut seqs = vec![];
		for i in 0..256u64 {
			if bitmap.bit(i as _).into() {
				println!("bit {} is set", i);
				seqs.push(start_seq + i);
			}
		}

		// next_ack is the sequence number used when acknowledging packets.
		// the value of next_ack is the sequence number of the next packet to be acknowledged yet.
		// aka the last acknowledged packet was next_ack - 1.

		// this function is called to calculate which acknowledgements have not yet been
		// relayed from this chain to the counterparty chain.
		Ok(seqs)
	}

	async fn query_unreceived_packets(
		&self,
		at: Height,
		channel_id: ChannelId,
		port_id: PortId,
		seqs: Vec<u64>,
	) -> Result<Vec<u64>, Self::Error> {
		let mut pending = vec![];

		for seq in seqs {
			let received = self
				.has_packet_receipt(at, port_id.as_str().to_owned(), format!("{channel_id}"), seq)
				.await?;

			if !received {
				pending.push(seq);
			}
		}

		Ok(pending)
	}

	async fn query_unreceived_acknowledgements(
		&self,
		at: Height,
		channel_id: ChannelId,
		port_id: PortId,
		seqs: Vec<u64>,
	) -> Result<Vec<u64>, Self::Error> {
		let mut pending = vec![];

		for seq in seqs {
			let received = !self
				.has_commitment(at, port_id.as_str().to_owned(), format!("{channel_id}"), seq)
				.await?;

			if !received {
				pending.push(seq);
			}
		}

		Ok(pending)
	}

	fn channel_whitelist(&self) -> HashSet<(ChannelId, PortId)> {
		self.channel_whitelist.lock().unwrap().clone().into_iter().collect()
	}

	async fn query_connection_channels(
		&self,
		at: Height,
		connection_id: &ConnectionId,
	) -> Result<QueryChannelsResponse, Self::Error> {
		unimplemented!("query_connection_channels")
	}

	async fn query_send_packets(
		&self,
		at: Height,
		channel_id: ChannelId,
		port_id: PortId,
		seqs: Vec<u64>,
	) -> Result<Vec<PacketInfo>, Self::Error> {
		let source_port = port_id.to_string();
		let source_channel = channel_id.to_string();
		let event_filter = self
			.yui
			.event_for_name::<SendPacketFilter>("SendPacket")
			.map_err(|err| ClientError::ContractAbiError(err))?
			.from_block(BlockNumber::Number(EARLIEST_BLOCK.into()))
			.address(ValueOrArray::Array(vec![
				self.yui.bank.as_ref().map(|x| x.address()).unwrap_or_default(),
				self.yui.ibc_core_diamond.address(),
			]))
			//            .address(ValueOrArray::Value(self.yui.diamond.address()))
			//.from_block(BlockNumber::Earliest) // TODO: use contract creation height
			.to_block(BlockNumber::Latest)
			.topic1(ValueOrArray::Array(
				seqs.clone()
					.into_iter()
					.map(|seq| {
						let bytes = encode(&[Token::Uint(seq.into())]);
						H256::from_slice(bytes.as_slice())
					})
					.collect(),
			))
			.topic2({
				let hash = H256::from_slice(&encode(&[Token::FixedBytes(
					keccak256(source_port.clone().into_bytes()).to_vec(),
				)]));
				ValueOrArray::Value(hash)
			})
			.topic3({
				let hash = H256::from_slice(&encode(&[Token::FixedBytes(
					keccak256(source_channel.clone().into_bytes()).to_vec(),
				)]));
				ValueOrArray::Value(hash)
			});

		for i in 0..4 {
			let Some(topic) = &event_filter.filter.topics[i] else { continue };
			let data = match topic {
				Topic::Value(v) => v.iter().map(|v| &v.0[..]).collect::<Vec<_>>(),
				Topic::Array(vs) => vs.iter().flatten().map(|v| &v.0[..]).collect(),
			};
			log::debug!(target: "hyperspace_ethereum",
				"Looking for topic{i}: {}",
				data.into_iter().map(hex::encode).collect::<Vec<_>>().join(", ")
			);
		}
		let mut logs = self
			.yui
			.ibc_core_diamond
			.client()
			.get_logs(&event_filter.filter)
			.await
			.map_err(|err| ClientError::Other(format!("failed to get logs 5: {}", err)))?;
		let logs2 = self
			.yui
			.bank
			.as_ref()
			.ok_or(ClientError::Other("bank contract not found".to_string()))?
			.client()
			.get_logs(&event_filter.filter)
			.await
			.map_err(|err| ClientError::Other(format!("failed to get logs 6: {}", err)))?;
		logs.extend(logs2);

		if logs.is_empty() {
			return Ok(vec![])
		}

		let channel = self.query_channel_end(at, channel_id, port_id).await?;

		let channel = channel.channel.ok_or(ClientError::Other("channel is none".to_string()))?;
		let counterparty = channel
			.counterparty
			.ok_or(ClientError::Other("counterparty is none".to_string()))?;

		let mut ret = vec![];
		for log in logs.into_iter() {
			let value = SendPacketFilter::decode_log(&log.clone().into())
				.map_err(|err| ClientError::Other(format!("failed to decode log: {}", err)))?;
			if !seqs.contains(&value.sequence) {
				continue
			}
			let packet_info = PacketInfo {
				height: Some(
					log.block_number
						.ok_or(ClientError::Other("block number not found".to_string()))?
						.as_u64()
						.into(),
				),
				source_port: source_port.clone(),
				source_channel: source_channel.clone(),
				destination_port: counterparty.port_id.clone(),
				destination_channel: counterparty.channel_id.clone(),
				sequence: value.sequence,
				timeout_height: value.timeout_height.into(),
				timeout_timestamp: value.timeout_timestamp,
				data: value.data.to_vec(),
				channel_order: Order::from_i32(channel.ordering)
					.map_err(|_| ClientError::Other("invalid channel order".to_string()))?
					.to_string(),
				ack: None,
			};
			ret.push(packet_info);
		}
		Ok(ret)
	}

	async fn query_received_packets(
		&self,
		at: Height,
		channel_id: ChannelId,
		port_id: PortId,
		seqs: Vec<u64>,
	) -> Result<Vec<PacketInfo>, Self::Error> {
		let destination_port = port_id.to_string();
		let destination_channel = channel_id.to_string();
		let event_filter = self
			.yui
			.event_for_name::<RecvPacketFilter>("RecvPacket")
			.map_err(|err| ClientError::ContractAbiError(err))?
			.from_block(BlockNumber::Number(EARLIEST_BLOCK.into()))
			.address(ValueOrArray::Value(self.yui.ibc_core_diamond.address()))
			//.from_block(BlockNumber::Earliest) // TODO: use contract creation height
			.to_block(BlockNumber::Latest)
			.topic1(ValueOrArray::Array(
				seqs.clone()
					.into_iter()
					.map(|seq| {
						let bytes = encode(&[Token::Uint(seq.into())]);
						H256::from_slice(bytes.as_slice())
					})
					.collect(),
			))
			.topic2({
				ValueOrArray::Value(H256::from_slice(&encode(&[Token::FixedBytes(
					keccak256(destination_port.clone().into_bytes()).to_vec(),
				)])))
			})
			.topic3({
				ValueOrArray::Value(H256::from_slice(&encode(&[Token::FixedBytes(
					keccak256(destination_channel.clone().into_bytes()).to_vec(),
				)])))
			});

		let logs = self
			.yui
			.ibc_core_diamond
			.client()
			.get_logs(&event_filter.filter)
			.await
			.map_err(|err| {
				ClientError::Other(format!("failed to get logs in query_received_packets: {}", err))
			})?;
		let channel = self.query_channel_end(at, channel_id, port_id).await?;
		let channel = channel.channel.ok_or(ClientError::Other("channel is none".to_string()))?;

		let acks_filter = self
			.yui
			.event_for_name::<WriteAcknowledgementFilter>("WriteAcknowledgement")
			.map_err(|err| ClientError::ContractAbiError(err))?
			.from_block(BlockNumber::Number(EARLIEST_BLOCK.into()))
			.address(ValueOrArray::Value(self.yui.ibc_core_diamond.address()))
			//.from_block(BlockNumber::Earliest) // TODO: use contract creation height
			.to_block(BlockNumber::Latest)
			.topic3(ValueOrArray::Array(
				seqs.clone()
					.into_iter()
					.map(|seq| {
						let bytes = encode(&[Token::Uint(seq.into())]);
						H256::from_slice(bytes.as_slice())
					})
					.collect(),
			))
			.topic1({
				ValueOrArray::Value(H256::from_slice(&encode(&[Token::FixedBytes(
					keccak256(destination_port.clone().into_bytes()).to_vec(),
				)])))
			})
			.topic2({
				ValueOrArray::Value(H256::from_slice(&encode(&[Token::FixedBytes(
					keccak256(destination_channel.clone().into_bytes()).to_vec(),
				)])))
			});

		let mut acks_map = acks_filter
			.query()
			.await
			.map_err(|err| {
				ClientError::Other(format!(
					"failed to get acks_map in query_received_packets: {}",
					err
				))
			})?
			.into_iter()
			.map(|ack| (ack.sequence, ack.acknowledgement.to_vec()))
			.collect::<HashMap<_, _>>();

		let mut ret = vec![];

		for log in logs.into_iter() {
			let value = RecvPacketFilter::decode_log(&log.clone().into()).map_err(|err| {
				ClientError::Other(format!("failed to decode log in query_send_packets: {}", err))
			})?;
			if !seqs.contains(&value.sequence) {
				continue
			}
			let packet_info = PacketInfo {
				height: Some(
					log.block_number
						.ok_or(ClientError::Other("block number not found".to_string()))?
						.as_u64()
						.into(),
				),
				source_port: value.source_port.clone(),
				source_channel: value.source_channel.clone(),
				destination_port: destination_port.clone(),
				destination_channel: destination_channel.clone(),
				sequence: value.sequence,
				timeout_height: value.timeout_height.into(),
				timeout_timestamp: value.timeout_timestamp,
				data: value.data.to_vec(),
				channel_order: Order::from_i32(channel.ordering)
					.map_err(|_| {
						Self::Error::Other("invalid channel order in query_send_packets".to_owned())
					})?
					.to_string(),
				ack: acks_map.get(&value.sequence).cloned(),
			};
			ret.push(packet_info);
		}

		Ok(ret)
	}

	fn expected_block_time(&self) -> Duration {
		Duration::from_secs(14)
	}

	async fn query_client_update_time_and_height(
		&self,
		client_id: ClientId,
		client_height: Height,
	) -> Result<(Height, Timestamp), Self::Error> {
		log::info!(target: "hyperspace_ethereum", "query_client_update_time_and_height: {client_id:?}, {client_height:?}");
		let event_filter = self
			.yui
			.event_for_name::<UpdateClientHeightFilter>("UpdateClientHeight")
			.map_err(|err| ClientError::ContractAbiError(err))?
			.from_block(BlockNumber::Number(EARLIEST_BLOCK.into()))
			.address(ValueOrArray::Value(self.yui.ibc_core_diamond.address()))
			//.from_block(BlockNumber::Earliest) // TODO: use contract creation height
			.to_block(BlockNumber::Latest)
			.topic1({
				ValueOrArray::Value(H256::from_slice(&encode(&[Token::FixedBytes(
					keccak256(client_id.to_string()).to_vec(),
				)])))
			})
			.topic2({
				let height_bytes = encode(&[Token::Tuple(vec![
					Token::Uint(client_height.revision_number.into()),
					Token::Uint(client_height.revision_height.into()),
				])]);
				ValueOrArray::Value(H256::from_slice(&encode(&[Token::FixedBytes(
					keccak256(&height_bytes).to_vec(),
				)])))
			});

		let log = self
			.yui
			.ibc_core_diamond
			.client()
			.get_logs(&event_filter.filter)
			.await
			.map_err(|err| {
				ClientError::Other(format!(
					"failed to get logs in query_client_update_time_and_height: {}",
					err
				))
			})?
			.pop()
			.ok_or_else(|| Self::Error::Other("no logs found".to_owned()))?;

		let height = Height::new(
			0,
			log.block_number
				.ok_or(ClientError::Other(
					"block number not found in query_client_update_time_and_height".to_string(),
				))?
				.as_u64(),
		);

		let timestamp =
			Timestamp::from_nanoseconds(self.query_timestamp_at(height.revision_height).await?)
				.map_err(|err| {
					ClientError::Other(format!(
						"failed to get timestamp in query_client_update_time_and_height: {}",
						err
					))
				})?;

		Ok((height, timestamp))
	}

	async fn query_host_consensus_state_proof(
		&self,
		_client_state: &AnyClientState,
	) -> Result<Option<Vec<u8>>, Self::Error> {
		Ok(Some(vec![]))
	}

	async fn query_ibc_balance(
		&self,
		asset_id: Self::AssetId,
	) -> Result<Vec<PrefixedCoin>, Self::Error> {
		let balance = self
			.yui
			.bank
			.as_ref()
			.ok_or(ClientError::Other("bank contract not found".to_string()))?
			.method::<_, U256>(
				"balanceOf",
				(
					H160::from_str(&self.account_id().to_string()).map_err(|_| {
						ClientError::Other("failed get bytes from account id".to_string())
					})?,
					asset_id.clone(),
				),
			)?
			.call()
			.await?;
		Ok(vec![PrefixedCoin {
			denom: PrefixedDenom {
				trace_path: TracePath::default(),
				base_denom: BaseDenom::from_str(&asset_id)?,
			},
			amount: Amount::from(balance),
		}])
	}

	fn connection_prefix(&self) -> CommitmentPrefix {
		CommitmentPrefix::try_from(self.config.commitment_prefix()).unwrap()
	}

	#[track_caller]
	fn client_id(&self) -> ClientId {
		self.client_id
			.lock()
			.unwrap()
			.as_ref()
			.expect("Client Id should be defined")
			.clone()
	}

	fn set_client_id(&mut self, client_id: ClientId) {
		*self.client_id.lock().unwrap() = Some(client_id);
	}

	fn connection_id(&self) -> Option<ConnectionId> {
		self.connection_id.lock().unwrap().clone()
	}

	fn set_channel_whitelist(&mut self, channel_whitelist: HashSet<(ChannelId, PortId)>) {
		*self.channel_whitelist.lock().unwrap() = channel_whitelist;
	}

	fn add_channel_to_whitelist(&mut self, channel: (ChannelId, PortId)) {
		self.channel_whitelist.lock().unwrap().insert(channel);
	}

	fn set_connection_id(&mut self, connection_id: ConnectionId) {
		*self.connection_id.lock().unwrap() = Some(connection_id);
	}

	fn client_type(&self) -> ClientType {
		"xx-ethereum".to_string()
	}

	async fn query_timestamp_at(&self, block_number: u64) -> Result<u64, Self::Error> {
		let block = self
			.client()
			.get_block(BlockId::Number(BlockNumber::Number(block_number.into())))
			.await
			.map_err(|err| ClientError::MiddlewareError(err))?
			.ok_or_else(|| ClientError::Other("block not found".to_string()))?;

		Ok(Duration::from_secs(block.timestamp.as_u64()).as_nanos() as u64)
	}

	// TODO: query_clients (ethereum)
	async fn query_clients(&self) -> Result<Vec<ClientId>, Self::Error> {
		Ok(vec![])
		// Ok(vec![ClientId::new("07-tendermint", 0).unwrap()])
	}

	async fn query_channels(&self) -> Result<Vec<(ChannelId, PortId)>, Self::Error> {
		// let ids = self.generated_channel_identifiers(0.into()).await?;
		// dbg!(&ids);
		// ids.into_iter()
		// 	.map(|id| Ok((id.1.parse().unwrap(), id.0.parse().unwrap())))
		// 	.collect()
		Ok(vec![])
	}

	async fn query_connection_using_client(
		&self,
		height: u32,
		client_id: String,
	) -> Result<Vec<IdentifiedConnection>, Self::Error> {
		Ok(vec![]) // TODO: query_connection_using_client (ethereum)
	}

	async fn is_update_required(
		&self,
		latest_height: u64,
		latest_client_height_on_counterparty: u64,
	) -> Result<bool, Self::Error> {
		Ok(false)
	}

	async fn initialize_client_state(
		&self,
	) -> Result<(AnyClientState, AnyConsensusState), Self::Error> {
		let sync_committee_prover = self.prover();
		let block_id = "head";
		let block_header = sync_committee_prover
			.fetch_header(&block_id)
			.await
			.map_err(|err| {
				ClientError::Other(format!(
					"failed to fetch header in initialize_client_state: {}",
					err
				))
			})
			.expect("1");

		let state = sync_committee_prover
			.fetch_beacon_state(block_id)
			.await
			.map_err(|err| {
				ClientError::Other(format!(
					"failed to fetch beacon state in initialize_client_state: {}",
					err
				))
			})
			.expect("2");

		// TODO: query `at` block
		// let finality_checkpoint =
		// sync_committee_prover.fetch_finalized_checkpoint().await.unwrap();

		let epoch = state.current_justified_checkpoint.epoch;
		let client_state = LightClientState {
			finalized_header: block_header.clone(),
			latest_finalized_epoch: epoch, // TODO: ????
			// latest_finalized_epoch: finality_checkpoint.finalized.epoch, // TODO: ????
			current_sync_committee: state.current_sync_committee,
			next_sync_committee: state.next_sync_committee,
		};

		let execution_header = state.latest_execution_payload_header;
		let block = self
			.client()
			.get_block(BlockId::Number(BlockNumber::Number(execution_header.block_number.into())))
			.await
			.map_err(|err| ClientError::MiddlewareError(err))?
			.ok_or(ClientError::Other(format!(
				"not able to find a block : {}",
				execution_header.block_number.to_string()
			)))?;

		log::info!(target: "hyperspace_ethereum", "Using init epoch: {epoch}, and height: {}", execution_header.block_number);

		let client_state = AnyClientState::Ethereum(ClientState {
			inner: client_state,
			frozen_height: None,
			latest_height: execution_header.block_number as _,
			// latest_height: block_header.slot as _
			_phantom: Default::default(),
		});

		let consensus_state = AnyConsensusState::Ethereum(ConsensusState {
			timestamp: tendermint::time::Time::from_unix_timestamp(
				execution_header.timestamp as i64,
				// block.timestamp.as_u64() as i64,
				0,
			)
			.map_err(|err| {
				ClientError::Other(format!(
					"failed to get timestamp in initialize_client_state: {}, timestamp{}",
					err, execution_header.timestamp
				))
			})?
			.into(),
			root: CommitmentRoot { bytes: execution_header.state_root.to_vec() },
			// root: CommitmentRoot { bytes: block.state_root.0.to_vec() },
		});

		Ok((client_state, consensus_state))
	}

	async fn query_client_id_from_tx_hash(
		&self,
		(block_hash, tx_hash): Self::TransactionId,
	) -> Result<ClientId, Self::Error> {
		let block_number = self
			.client()
			.get_block(BlockId::Hash(block_hash))
			.await
			.map_err(|err| ClientError::MiddlewareError(err))?
			.ok_or(ClientError::Other(format!(
				"not able to find a block : {}",
				block_hash.to_string()
			)))?
			.number
			.ok_or(ClientError::Other(format!(
				"not able to find a block number with hash: {}",
				block_hash.to_string()
			)))?;
		let event_filter = self
			.yui
			.event_for_name::<GeneratedClientIdentifierFilter>("GeneratedClientIdentifier")
			.map_err(|err| ClientError::ContractAbiError(err))?
			.from_block(block_number)
			.to_block(block_number);
		let log = self
			.yui
			.ibc_core_diamond
			.client()
			.get_logs(&event_filter.filter)
			.await
			.map_err(|err| ClientError::MiddlewareError(err))?
			.into_iter()
			.find(|log| log.transaction_hash == Some(tx_hash))
			.ok_or(ClientError::Other(format!(
				"not able to find a log with tx hash: {}",
				tx_hash.to_string()
			)))?;

		let decoded_log = GeneratedClientIdentifierFilter::decode_log(&log.clone().into())
			.map_err(|err| {
				ClientError::Other(format!(
					"failed to decode log in query_client_id_from_tx_hash: {}",
					err
				))
			})?;
		Ok(decoded_log.0.parse()?)
	}

	async fn query_connection_id_from_tx_hash(
		&self,
		(block_hash, tx_hash): Self::TransactionId,
	) -> Result<ConnectionId, Self::Error> {
		let block_number = self
			.client()
			.get_block(BlockId::Hash(block_hash))
			.await
			.map_err(|err| ClientError::MiddlewareError(err))?
			.ok_or(ClientError::Other(format!(
				"not able to find a block with hash: {}",
				block_hash.to_string()
			)))?
			.number
			.ok_or(ClientError::Other(format!(
				"not able to find a block number with hash: {}",
				block_hash.to_string()
			)))?;
		let event_filter = self
			.yui
			.event_for_name::<OpenInitConnectionFilter>("OpenInitConnection")
			.map_err(|err| ClientError::ContractAbiError(err))?
			.from_block(block_number)
			.to_block(block_number);
		let log = self
			.yui
			.ibc_core_diamond
			.client()
			.get_logs(&event_filter.filter)
			.await
			.map_err(|err| ClientError::MiddlewareError(err))?
			.into_iter()
			.find(|log| log.transaction_hash == Some(tx_hash))
			.ok_or(ClientError::Other(format!(
				"not able to find a log with tx hash: {}",
				tx_hash.to_string()
			)))?;

		let decoded_log = OpenInitConnectionFilter::decode_log(&log.clone().into()).map_err(
			|err| ClientError::Other(format!("failed to decode OpenInitConnectionFilter in query_connection_id_from_tx_hash: {}", err)),
		)?;
		Ok(decoded_log.connection_id.parse()?)
	}

	async fn query_channel_id_from_tx_hash(
		&self,
		(block_hash, tx_hash): Self::TransactionId,
	) -> Result<(ChannelId, PortId), Self::Error> {
		let block_number = self
			.client()
			.get_block(BlockId::Hash(block_hash))
			.await
			.map_err(|err| ClientError::MiddlewareError(err))?
			.ok_or(ClientError::Other(format!(
				"not able to find a block with hash: {}",
				block_hash.to_string()
			)))?
			.number
			.ok_or(ClientError::Other(format!(
				"not able to find a block number with hash: {}",
				block_hash.to_string()
			)))?;
		let event_filter = self
			.yui
			.event_for_name::<OpenInitChannelFilter>("OpenInitChannel")
			.map_err(|err| ClientError::ContractAbiError(err))?
			.from_block(block_number)
			.to_block(block_number);
		let log = self
			.yui
			.ibc_core_diamond
			.client()
			.get_logs(&event_filter.filter)
			.await
			.map_err(|err| ClientError::MiddlewareError(err))?
			.into_iter()
			.find(|log| {
				if let Some(transaction_hash) = &log.transaction_hash {
					*transaction_hash == tx_hash
				} else {
					log::error!(target: "hyperspace_ethereum", "tx hash should exist: {tx_hash}");
					return false
				}
			})
			.ok_or(ClientError::Other(format!(
				"not able to find a log with tx hash: {}",
				tx_hash.to_string()
			)))?;

		let decoded_log =
			OpenInitChannelFilter::decode_log(&log.clone().into()).map_err(|err| {
				ClientError::Other(format!(
					"failed to decode OpenInitChannelFilter in query_channel_id_from_tx_hash: {}",
					err
				))
			})?;
		Ok((decoded_log.channel_id.parse()?, decoded_log.port_id.parse()?))
	}

	async fn upload_wasm(&self, _wasm: Vec<u8>) -> Result<Vec<u8>, Self::Error> {
		unimplemented!("upload_wasm")
	}
}