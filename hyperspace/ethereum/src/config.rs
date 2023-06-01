use std::str::FromStr;

use ethers::types::Address;
use ibc::core::ics24_host::identifier::{ChannelId, ClientId, ConnectionId, PortId};
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

fn address_de<'de, D>(de: D) -> Result<Address, D::Error> where D: Deserializer<'de> {
	struct FromStr;

	impl Visitor<'_> for FromStr {
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

	de.deserialize_str(FromStr)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
	/// HTTP URL for RPC
	#[serde(deserialize_with = "uri_de", serialize_with = "uri_se")]
	pub http_rpc_url: http::uri::Uri,
	/// Websocket URL for RPC
	#[serde(deserialize_with = "uri_de", serialize_with = "uri_se")]
	pub ws_rpc_url: http::uri::Uri,
	/// address of the OwnableIBCHandler contract.
	#[serde(deserialize_with = "address_de")]
	pub ibc_handler_address: Address,
	/// address of the IBCPacket contract.
	#[serde(deserialize_with = "address_de")]
	pub ibc_packet_address: Address,
	/// address of the IBCClient contract.
	#[serde(deserialize_with = "address_de")]
	pub ibc_client_address: Address,
	/// address of the IBCConnection contract.
	#[serde(deserialize_with = "address_de")]
	pub ibc_connection_address: Address,
	/// address of the IBCChannelHandshake contract.
	#[serde(deserialize_with = "address_de")]
	pub ibc_channel_handshake_address: Address,
	/// mnemonic for the wallet
	pub mnemonic: String,
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
}

impl Config {
	/// ICS-23 compatible commitment prefix
	#[track_caller]
	pub fn commitment_prefix(&self) -> Vec<u8> {
		hex::decode(self.commitment_prefix.clone()).expect("bad commitment prefix hex")
	}
}

#[cfg(test)]
mod test;
