use std::sync::Arc;

use ethers::{
	abi::{Abi, Address, Detokenize, Token},
	prelude::Contract,
	providers::Middleware,
};

/// Unwraps a contract error, decoding the revert reason if possible
pub trait UnwrapContractError<T> {
	fn unwrap_contract_error(self) -> T;
}

impl<T, M> UnwrapContractError<T> for Result<T, ethers::prelude::ContractError<M>>
where
	M: Middleware,
{
	/// Unwraps a contract error, decoding the revert reason if possible
	#[track_caller]
	fn unwrap_contract_error(self) -> T {
		match self {
			Ok(t) => t,
			Err(ethers::prelude::ContractError::Revert(bytes)) => {
				// abi decode the bytes after the first 4 bytes (the error selector)
				if bytes.len() < 4 {
					panic!("contract-error: {:?}", bytes);
				}
				let bytes = &bytes[4..];
				let tokens = ethers::abi::decode(&[ethers::abi::ParamType::String], bytes).unwrap();
				panic!("contract-error: {tokens:#?}")
			},
			Err(e) => panic!("contract-error: {:?}", e),
		}
	}
}


pub const IBC_HANDLER_ABI: &str = include_str!("./abi/ibc-handler-abi.json");

/// A wrapper around the IBC handler contract instance
pub struct IbcHandler<M> {
	pub(crate) contract: Contract<M>,
}

impl<M> IbcHandler<M>
where
	M: Middleware,
{
	pub async fn bind_port(&self, port_id: &str, address: Address) {
		let bind_port = self
			.contract
			.method::<_, ()>("bindPort", (Token::String(port_id.into()), Token::Address(address)))
			.unwrap();
		let () = bind_port.call().await.unwrap_contract_error();
		let tx_recp = bind_port.send().await.unwrap_contract_error().await.unwrap().unwrap();
		assert_eq!(tx_recp.status, Some(1.into()));
	}

	pub async fn connection_open_init(&self, client_id: &str) -> String {
		let connection_open_init = self
			.contract
			.method::<_, String>(
				"connectionOpenInit",
				(Token::Tuple(vec![
					Token::String(client_id.into()),
					Token::Tuple(vec![
						Token::String(client_id.into()),
						Token::String("port-0".into()),
						Token::Tuple(vec![Token::Bytes(vec![])]),
					]),
					Token::Uint(0.into()),
				]),),
			)
			.unwrap();
		let connection_id = connection_open_init.call().await.unwrap_contract_error();
		let tx_recp = connection_open_init
			.send()
			.await
			.unwrap_contract_error()
			.await
			.unwrap()
			.unwrap();
		assert_eq!(tx_recp.status, Some(1.into()));
		connection_id
	}

	pub async fn connection_open_ack(&self, connection_id: &str, client_state_bytes: Vec<u8>) {
		let connection_open_ack = self
			.contract
			.method::<_, ()>(
				"connectionOpenAck",
				(Token::Tuple(vec![
					Token::String(connection_id.to_string()),
					Token::Bytes(client_state_bytes), // clientStateBytes
					Token::Tuple(vec![
						Token::String("counterparty-version".into()),
						Token::Array(vec![]),
					]), // Version.Data
					Token::String("counterparty-connection-id".into()), // counterpartyConnectionID
					Token::Bytes(vec![]),             // proofTry
					Token::Bytes(vec![]),             // proofClient
					Token::Bytes(vec![]),             // proofConsensus
					Token::Tuple(vec![Token::Uint(0.into()), Token::Uint(1.into())]), // proofHeight
					Token::Tuple(vec![Token::Uint(0.into()), Token::Uint(1.into())]), // consesusHeight
				]),),
			)
			.unwrap();

		let () = connection_open_ack.call().await.unwrap_contract_error();
		let tx_recp =
			connection_open_ack.send().await.unwrap_contract_error().await.unwrap().unwrap();
		assert_eq!(tx_recp.status, Some(1.into()));
	}

	pub async fn channel_open_try(&self, connection_id: &str, port_id: &str) -> String {
		let channel_open_try = self
			.contract
			.method::<_, String>(
				"channelOpenTry",
				(Token::Tuple(vec![
					Token::String(port_id.into()), // port-id
					Token::Tuple(vec![
						// Channel.Data
						Token::Uint(2.into()), //  state, 1: TryOpen
						Token::Uint(1.into()), //  ordering, 1: Unordered
						Token::Tuple(vec![
							//  ChannelCounterparty.Data
							Token::String(Default::default()), // port-id
							Token::String(Default::default()), // channel-id
						]),
						Token::Array(vec![Token::String(connection_id.into())]), // connectionHops
						Token::String("1".into()),                               // version
					]),
					Token::String("1".into()), // counterpartyVersion
					Token::Bytes(vec![]),      // proofInit
					Token::Tuple(vec![
						// proofHeight
						Token::Uint(0.into()), //  revisionNumber
						Token::Uint(1.into()), //  revisionHeight
					]),
				]),),
			)
			.unwrap();

		let channel_id = channel_open_try.call().await.unwrap_contract_error();
		let tx_recp = channel_open_try.send().await.unwrap_contract_error().await.unwrap().unwrap();
		assert_eq!(tx_recp.status, Some(1.into()));
		channel_id
	}

	pub async fn register_client(&self, kind: &str, address: Address) {
		let method = self
			.contract
			.method::<_, ()>(
				"registerClient",
				(Token::String(kind.into()), Token::Address(address)),
			)
			.unwrap();

		let receipt = method.send().await.unwrap().await.unwrap().unwrap();
		assert_eq!(receipt.status, Some(1.into()));
	}

	pub async fn create_client(&self, msg: Token) -> String {
		let method = self.contract.method::<_, String>("createClient", (msg,)).unwrap();

		let client_id = method.call().await.unwrap_contract_error();

		let receipt = method.send().await.unwrap().await.unwrap().unwrap();
		assert_eq!(receipt.status, Some(1.into()));

		client_id
	}
}

/// Create a new contract instance from the given address and ABI.
#[track_caller]
pub fn ibc_handler<M>(address: Address, client: Arc<M>) -> Contract<M>
where
	M: Middleware,
{
	let abi: Abi = serde_json::from_str(IBC_HANDLER_ABI).unwrap();
	Contract::new(address, abi, client)
}

pub(crate) struct Counterparty {
	pub(crate) client_id: String,
	pub(crate) connection_id: String,
	pub(crate) prefix: Vec<u8>,
}

pub(crate) struct Version {
	pub(crate) identifier: String,
	pub(crate) features: Vec<String>,
}

pub struct ChannelEnd {
	pub(crate) state: u32,
	pub(crate) ordering: u32,
	pub(crate) counterparty: Counterparty,
	pub(crate) connection_hops: Vec<String>,
	pub(crate) version: String,
}

impl Detokenize for ChannelEnd {
	fn from_tokens(tokens: Vec<ethers::abi::Token>) -> Result<Self, ethers::abi::InvalidOutputType>
	where
		Self: Sized,
	{
		let vec = tokens[3].clone().into_array().unwrap();

		let connection_hops = {
			let mut it = vec.into_iter();
			let mut v = vec![];

			while let Some(connection_id) = it.next() {
				v.push(connection_id.into_string().unwrap())
			}

			v
		};

		Ok(ChannelEnd {
			state: tokens[0].clone().into_uint().unwrap().as_u32(),
			ordering: tokens[1].clone().into_uint().unwrap().as_u32(),
			counterparty: Counterparty {
				client_id: tokens[2].clone().into_string().unwrap(),
				connection_id: tokens[3].clone().into_string().unwrap(),
				prefix: tokens[4].clone().into_bytes().unwrap(),
			},
			connection_hops,
			version: tokens[5].clone().into_string().unwrap(),
		})
	}
}

pub struct ConnectionEnd {
	pub(crate) client_id: String,
	pub(crate) versions: Vec<Version>,
	pub(crate) state: u32,
	pub(crate) counterparty: Counterparty,
	pub(crate) delay_period: u64,
}

impl Detokenize for ConnectionEnd {
	fn from_tokens(tokens: Vec<ethers::abi::Token>) -> Result<Self, ethers::abi::InvalidOutputType>
	where
		Self: Sized,
	{
		let vec = tokens[1].clone().into_array().unwrap();

		let versions = {
			let mut it = vec.into_iter();
			let mut v = vec![];

			while let (Some(identifier), Some(features)) = (it.next(), it.next()) {
				v.push(Version {
					identifier: identifier.into_string().unwrap(),
					features: features
						.into_array()
						.unwrap()
						.into_iter()
						.map(|t| t.into_string().unwrap())
						.collect(),
				})
			}

			v
		};

		Ok(ConnectionEnd {
			client_id: tokens[0].clone().into_string().unwrap(),
			versions,
			state: tokens[2].clone().into_uint().unwrap().as_u32(),
			counterparty: Counterparty {
				client_id: tokens[3].clone().into_string().unwrap(),
				connection_id: tokens[4].clone().into_string().unwrap(),
				prefix: tokens[5].clone().into_bytes().unwrap(),
			},
			delay_period: tokens[4].clone().into_uint().unwrap().as_u64(),
		})
	}
}

pub const LIGHT_CLIENT_ABI_JSON: &str = include_str!("./abi/light-client-abi.json");

/// Create a new contract instance from the given address and ABI.
#[track_caller]
pub fn light_client_contract<M>(address: Address, client: Arc<M>) -> Contract<M>
where
	M: Middleware,
{
	let abi: Abi = serde_json::from_str(LIGHT_CLIENT_ABI_JSON).unwrap();
	Contract::new(address, abi, client)
}
