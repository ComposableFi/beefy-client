use crate::cosmos::{client::CosmosClient, client::CosmosClientConfig, key_provider::KeyEntry};
use futures::{future, StreamExt};
use hyperspace_core::logging;
use hyperspace_cosmos::{key_provider::KeyEntry, CosmosClient, CosmosClientConfig, IbcProvider};
use hyperspace_primitives::{utils::create_clients, IbcProvider};
use hyperspace_testsuite::{
	ibc_channel_close, ibc_messaging_packet_height_timeout_with_connection_delay,
	ibc_messaging_packet_timeout_on_channel_close,
	ibc_messaging_packet_timestamp_timeout_with_connection_delay,
	ibc_messaging_with_connection_delay,
};
use ibc::events::IbcEvent;
use ibc::{
	applications::transfer::VERSION,
	core::{
		ics04_channel::channel::{ChannelEnd, Order, State},
		ics24_host::identifier::{ChannelId, ConnectionId, PortId},
	},
};
use std::time::Duration;
use std::{path::PathBuf, str::FromStr};
use tendermint_rpc::Url;
use tokio::task::JoinHandle;

async fn setup_clients<H: Clone + Send + Sync + 'static>() -> (CosmosClient<H>, CosmosClient<H>) {
	log::info!(target: "hyperspace", "=========================== Starting Test ===========================");

	// Create client configurations
	// Parameters have been set up to work with local nodes according to https://hermes.informal.systems/tutorials
	let config_a = CosmosClientConfig {
		name: "chain_a".to_string(),
		rpc_url: Url::from_str("http://127.0.0.1:27030").unwrap(),
		grpc_url: Url::from_str("http://127.0.0.1:27032").unwrap(),
		websocket_url: Url::from_str("ws://127.0.0.1:27030/websocket").unwrap(),
		chain_id: "ibc-0".to_string(),
		client_id: Some("7-tendermint".to_string()),
		connection_id: None,
		account_prefix: "cosmos".to_string(),
		store_prefix: "ibc".to_string(),
		keybase: KeyEntry::from_file(
			PathBuf::from_str("keys/ibc-0/keyring-test/wallet.json").unwrap(),
		)
		.unwrap(),
	};

	let config_b = CosmosClientConfig {
		name: "chain_b".to_string(),
		rpc_url: Url::from_str("http://127.0.0.1:27040").unwrap(),
		grpc_url: Url::from_str("http://127.0.0.1:27042").unwrap(),
		websocket_url: Url::from_str("ws://127.0.0.1:27040/websocket").unwrap(),
		chain_id: "ibc-1".to_string(),
		client_id: Some("7-tendermint".to_string()),
		connection_id: None,
		account_prefix: "cosmos".to_string(),
		store_prefix: "ibc".to_string(),
		keybase: KeyEntry::from_file(
			PathBuf::from_str("keys/ibc-1/keyring-test/wallet.json").unwrap(),
		)
		.unwrap(),
	};

	let mut chain_a = CosmosClient::<H>::new(config_a).await.unwrap();
	let mut chain_b = CosmosClient::<H>::new(config_b).await.unwrap();

	// Wait until for cosmos to start producing blocks
	log::info!(target: "hyperspace", "Waiting for block production from Cosmos chains");
	let _ = chain_a
		.ibc_events()
		.await
		.skip_while(|ev| future::ready(!matches!(ev, IbcEvent::NewBlock(_))))
		.take(1)
		.collect::<Vec<_>>()
		.await;
	let _ = chain_b
		.ibc_events()
		.await
		.skip_while(|ev| future::ready(!matches!(ev, IbcEvent::NewBlock(_))))
		.take(1)
		.collect::<Vec<_>>()
		.await;
	log::info!(target: "hyperspace", "Cosmos chains are ready to go!");

	// Check if the clients are already created
	let clients_on_a = chain_a.query_clients().await.unwrap();
	let clients_on_b = chain_b.query_clients().await.unwrap();

	if !clients_on_a.is_empty() && !clients_on_b.is_empty() {
		chain_a.set_client_id(clients_on_b[0].clone());
		chain_b.set_client_id(clients_on_b[0].clone());
		return (chain_a, chain_b);
	}

	let (client_a, client_b) = create_clients(&chain_a, &chain_b).await.unwrap();
	chain_a.set_client_id(client_a);
	chain_b.set_client_id(client_b);

	(chain_a, chain_b)
}

#[tokio::test]
async fn cosmos_to_cosmos_ibc_messaging_full_integration_test() {
	logging::setup_logging();
	let (mut chain_a, mut chain_b) = setup_clients::<()>().await;
	// Run tests sequentially

	// no timeouts + connection delay
	// ibc_messaging_with_connection_delay(&mut chain_a, &mut chain_b).await;

	// // timeouts + connection delay
	// ibc_messaging_packet_height_timeout_with_connection_delay(&mut chain_a, &mut chain_b).await;
	// ibc_messaging_packet_timestamp_timeout_with_connection_delay(&mut chain_a, &mut
	// chain_b).await;

	// // channel closing semantics
	// ibc_messaging_packet_timeout_on_channel_close(&mut chain_a, &mut chain_b).await;
	// ibc_channel_close(&mut chain_a, &mut chain_b).await;
}
