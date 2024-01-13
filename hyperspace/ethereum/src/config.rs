use ethers::{
	abi::Abi,
	core::k256,
	middleware::SignerMiddleware,
	prelude::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer},
};
use std::{
	fmt::{Debug, Display, Formatter},
	str::FromStr,
	sync::{Arc, Mutex},
};

use crate::{
	client::{ClientError, EthRpcClient},
	ibc_provider::{
		DIAMONDABI_ABI, DIAMONDCUTFACETABI_ABI, DIAMONDLOUPEFACETABI_ABI, ERC20TOKENABI_ABI,
		GOVERNANCEFACETABI_ABI, IBCCHANNELABI_ABI, IBCCLIENTABI_ABI, IBCCONNECTIONABI_ABI,
		IBCPACKETABI_ABI, IBCQUERIERABI_ABI, ICS20BANKABI_ABI, ICS20TRANSFERBANKABI_ABI,
		OWNERSHIPFACETABI_ABI, RELAYERWHITELISTFACETABI_ABI, TENDERMINTCLIENTABI_ABI,
	},
	utils::{DeployYuiIbc, ProviderImpl},
};
use ethers::{types::Address, utils::AnvilInstance};
use ethers_providers::{Http, Middleware, Provider};
use ibc::core::ics24_host::identifier::{ChannelId, ClientId, ConnectionId, PortId};
use primitives::CommonClientConfig;
use serde::{de::Visitor, Deserialize, Deserializer, Serialize, Serializer};

fn uri_de<'de, D>(de: D) -> Result<http::uri::Uri, D::Error>
where
	D: Deserializer<'de>,
{
	struct FromStr;

	impl Visitor<'_> for FromStr {
		type Value = http::uri::Uri;

		fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
			write!(formatter, "a string that can parse into a http URI")
		}

		fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
		where
			E: serde::de::Error,
		{
			http::uri::Uri::from_str(&v).map_err(serde::de::Error::custom)
		}
	}

	de.deserialize_str(FromStr)
}

fn uri_se<S>(uri: &http::uri::Uri, ser: S) -> Result<S::Ok, S::Error>
where
	S: Serializer,
{
	ser.serialize_str(&format!("{uri}"))
}

struct AddressFromStr;

impl Visitor<'_> for AddressFromStr {
	type Value = Address;

	fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
		write!(formatter, "a string that can parse into an address")
	}

	fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
	where
		E: serde::de::Error,
	{
		Address::from_str(&v).map_err(serde::de::Error::custom)
	}
}

fn address_de<'de, D>(de: D) -> Result<Address, D::Error>
where
	D: Deserializer<'de>,
{
	de.deserialize_str(AddressFromStr)
}

fn address_de_opt<'de, D>(de: D) -> Result<Option<Address>, D::Error>
where
	D: Deserializer<'de>,
{
	de.deserialize_str(AddressFromStr).map(Some)
}

#[derive(Clone, Deserialize, Serialize)]
pub struct EthereumClientConfig {
	/// HTTP URL for RPC
	#[serde(deserialize_with = "uri_de", serialize_with = "uri_se")]
	pub http_rpc_url: http::uri::Uri,
	/// Websocket URL for RPC
	#[serde(deserialize_with = "uri_de", serialize_with = "uri_se")]
	pub ws_rpc_url: http::uri::Uri,
	/// HTTP URL for RPC (Beacon node)
	#[serde(deserialize_with = "uri_de", serialize_with = "uri_se")]
	pub beacon_rpc_url: http::uri::Uri,
	/// mnemonic for the wallet
	pub mnemonic: Option<String>,
	/// private key for the wallet
	pub private_key: Option<String>,
	/// private key path for the wallet
	pub private_key_path: Option<String>,
	/// maximum block weight
	pub max_block_weight: u64,
	/// Name of the chain
	pub name: String,
	/// Light client id on counterparty chain
	pub client_id: Option<ClientId>,
	/// Connection Id
	pub connection_id: Option<ConnectionId>,
	/// Whitelisted channels
	pub channel_whitelist: Vec<(ChannelId, PortId)>,
	/// Commitment prefix
	pub commitment_prefix: String,
	/// All the client states and headers will be wrapped in WASM ones using the WASM code ID.
	#[serde(default)]
	pub wasm_code_id: Option<String>,
	/// Diamond contract address
	#[serde(deserialize_with = "address_de_opt")]
	#[serde(default)]
	pub diamond_address: Option<Address>,
	/// ICS-07 Tendermint light-client contract address
	#[serde(deserialize_with = "address_de_opt")]
	#[serde(default)]
	pub tendermint_address: Option<Address>,
	/// Government proxy contract address
	#[serde(deserialize_with = "address_de_opt")]
	#[serde(default)]
	pub gov_proxy_address: Option<Address>,
	/// ICS-20 Bank address
	#[serde(deserialize_with = "address_de_opt")]
	#[serde(default)]
	pub ics20_transfer_bank_address: Option<Address>,
	#[serde(deserialize_with = "address_de_opt")]
	#[serde(default)]
	pub ics20_bank_address: Option<Address>,
	/// Diamond facets (ABI file name, contract address)
	#[serde(default)]
	pub diamond_facets: Vec<(ContractName, Address)>,
	#[serde(skip)]
	pub yui: Option<DeployYuiIbc<Arc<ProviderImpl>, ProviderImpl>>,
	pub client_type: String,
	pub jwt_secret_path: Option<String>,
	pub indexer_pg_url: String,
	pub indexer_redis_url: String,
	#[serde(skip)]
	pub anvil: Option<Arc<Mutex<AnvilInstance>>>,
	/// Common client config
	#[serde(flatten)]
	pub common: CommonClientConfig,
}

impl Debug for EthereumClientConfig {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("EthereumClientConfig")
			.field("http_rpc_url", &self.http_rpc_url)
			.field("ws_rpc_url", &self.ws_rpc_url)
			.field("beacon_rpc_url", &self.beacon_rpc_url)
			.field("mnemonic", &self.mnemonic)
			.field("private_key", &self.private_key)
			.field("private_key_path", &self.private_key_path)
			.field("max_block_weight", &self.max_block_weight)
			.field("name", &self.name)
			.field("client_id", &self.client_id)
			.field("connection_id", &self.connection_id)
			.field("channel_whitelist", &self.channel_whitelist)
			.field("commitment_prefix", &self.commitment_prefix)
			.field("wasm_code_id", &self.wasm_code_id)
			.field("diamond_address", &self.diamond_address)
			.field("tendermint_address", &self.tendermint_address)
			.field("ics20_transfer_bank_address", &self.ics20_transfer_bank_address)
			.field("ics20_bank_address", &self.ics20_bank_address)
			.field("diamond_facets", &self.diamond_facets)
			.field("yui", &self.yui)
			.field("client_type", &self.client_type)
			.field("jwt_secret_path", &self.jwt_secret_path)
			.field("indexer_pg_url", &self.indexer_pg_url)
			.field("indexer_redis_url", &self.indexer_redis_url)
			.finish()
	}
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, strum::EnumString, PartialEq, Eq, Hash)]
pub enum ContractName {
	Diamond,
	DiamondCutFacet,
	DiamondLoupeFacet,
	IBCChannelHandshake,
	IBCClient,
	IBCConnection,
	IBCPacket,
	IBCQuerier,
	ICS20Bank,
	ICS20TransferBank,
	OwnershipFacet,
	TendermintLightClientZK,
	ERC20Token,
	GovernanceFacet,
	RelayerWhitelistFacet,
}

impl Display for ContractName {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "{self:?}")
	}
}

impl ContractName {
	pub fn to_abi(&self) -> Abi {
		match self {
			ContractName::Diamond => DIAMONDABI_ABI.clone(),
			ContractName::DiamondCutFacet => DIAMONDCUTFACETABI_ABI.clone(),
			ContractName::DiamondLoupeFacet => DIAMONDLOUPEFACETABI_ABI.clone(),
			ContractName::IBCChannelHandshake => IBCCHANNELABI_ABI.clone(),
			ContractName::IBCClient => IBCCLIENTABI_ABI.clone(),
			ContractName::IBCConnection => IBCCONNECTIONABI_ABI.clone(),
			ContractName::IBCPacket => IBCPACKETABI_ABI.clone(),
			ContractName::IBCQuerier => IBCQUERIERABI_ABI.clone(),
			ContractName::ICS20TransferBank => ICS20TRANSFERBANKABI_ABI.clone(),
			ContractName::ICS20Bank => ICS20BANKABI_ABI.clone(),
			ContractName::OwnershipFacet => OWNERSHIPFACETABI_ABI.clone(),
			ContractName::TendermintLightClientZK => TENDERMINTCLIENTABI_ABI.clone(),
			ContractName::ERC20Token => ERC20TOKENABI_ABI.clone(),
			ContractName::GovernanceFacet => GOVERNANCEFACETABI_ABI.clone(),
			ContractName::RelayerWhitelistFacet => RELAYERWHITELISTFACETABI_ABI.clone(),
		}
	}
}

impl EthereumClientConfig {
	/// ICS-23 compatible commitment prefix
	#[track_caller]
	pub fn commitment_prefix(&self) -> Vec<u8> {
		hex::decode(self.commitment_prefix.clone()).expect("bad commitment prefix hex")
	}

	pub async fn client(&self) -> Result<Arc<EthRpcClient>, ClientError> {
		let client = Provider::<Http>::try_from(self.http_rpc_url.to_string())
			.map_err(|_| ClientError::UriParseError(self.http_rpc_url.clone()))?;

		let chain_id = client.get_chainid().await.unwrap();

		let wallet: LocalWallet = if let Some(mnemonic) = &self.mnemonic {
			MnemonicBuilder::<English>::default().phrase(mnemonic.as_str()).build().unwrap()
		} else if let Some(path) = self.private_key_path.clone() {
			LocalWallet::decrypt_keystore(
				path,
				std::env::var("KEY_PASS").expect("KEY_PASS is not set"),
			)
			.unwrap()
			.into()
		} else if let Some(private_key) = self.private_key.clone() {
			let key =
				elliptic_curve::SecretKey::<k256::Secp256k1>::from_sec1_pem(private_key.as_str())
					.unwrap();
			key.into()
		} else {
			panic!("no private key or mnemonic provided")
		};

		Ok(Arc::new(SignerMiddleware::new(client, wallet.with_chain_id(chain_id.as_u64()))))
	}
}

#[cfg(test)]
mod test;