use crate::{
	client::{ClientError, EthereumClient},
	ibc_provider::{
		AcknowledgePacketFilter, CloseConfirmChannelFilter, CloseInitChannelFilter,
		OpenAckChannelFilter, OpenAckConnectionFilter, OpenConfirmChannelFilter,
		OpenConfirmConnectionFilter, OpenInitChannelFilter, OpenInitConnectionFilter,
		OpenTryConnectionFilter, PacketData, SendPacketFilter, TimeoutOnClosePacketFilter,
		TimeoutPacketFilter, WriteAcknowledgementFilter,
	},
};
use async_trait::async_trait;
use ethers::prelude::Log;
use ibc::{
	core::{
		ics02_client::events::{Attributes as ClientAttributes, CreateClient, UpdateClient},
		ics03_connection::events::{
			self as connection, Attributes, OpenConfirm as ConnectionOpenConfirm,
		},
		ics04_channel::{
			events::{self as channel, CloseConfirm, OpenConfirm as ChannelOpenConfirm},
			packet::{Packet, Sequence},
		},
		ics24_host::identifier::{ChannelId, ConnectionId, PortId},
	},
	events::IbcEvent,
	timestamp::Timestamp,
	Height,
};
use primitives::IbcProvider;

#[async_trait]
pub trait TryFromEvent<T>
where
	Self: Sized,
{
	async fn try_from_event(
		client: &EthereumClient,
		event: T,
		log: Log,
		height: Height,
	) -> Result<Self, ClientError>;
}

// TODO: UpdateClient, CreateClient event parsing

#[async_trait]
impl TryFromEvent<OpenConfirmConnectionFilter> for IbcEvent {
	async fn try_from_event(
		client: &EthereumClient,
		event: OpenConfirmConnectionFilter,
		_log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		let OpenConfirmConnectionFilter { connection_id } = event;
		let connection_id: ConnectionId = connection_id.parse()?;
		let resp = client.query_connection_end(height, connection_id.clone()).await?;
		let counterparty = resp.connection.unwrap().counterparty.unwrap();
		Ok(IbcEvent::OpenConfirmConnection(ConnectionOpenConfirm(Attributes {
			height,
			connection_id: Some(connection_id),
			client_id: client.client_id(),
			counterparty_connection_id: Some(counterparty.connection_id.parse()?),
			counterparty_client_id: counterparty.client_id.parse()?,
		})))
	}
}

#[async_trait]
impl TryFromEvent<OpenConfirmChannelFilter> for IbcEvent {
	async fn try_from_event(
		client: &EthereumClient,
		event: OpenConfirmChannelFilter,
		_log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		let OpenConfirmChannelFilter { port_id, channel_id } = event;
		let port_id: PortId = port_id.parse()?;
		let channel_id: ChannelId = channel_id.parse()?;
		let resp = client.query_channel_end(height, channel_id, port_id.clone()).await?;
		let channel = resp.channel.unwrap();
		let counterparty = channel.counterparty.unwrap();
		Ok(IbcEvent::OpenConfirmChannel(ChannelOpenConfirm {
			height,
			channel_id: Some(channel_id),
			connection_id: channel.connection_hops[0].parse()?,
			counterparty_port_id: counterparty.port_id.parse()?,
			port_id,
			counterparty_channel_id: Some(counterparty.port_id.parse()?),
		}))
	}
}

#[async_trait]
impl TryFromEvent<OpenInitConnectionFilter> for IbcEvent {
	async fn try_from_event(
		_client: &EthereumClient,
		event: OpenInitConnectionFilter,
		_log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		let OpenInitConnectionFilter {
			connection_id,
			client_id,
			counterparty_connection_id,
			counterparty_client_id,
		} = event;
		let connection_id: ConnectionId = connection_id.parse()?;
		Ok(IbcEvent::OpenInitConnection(connection::OpenInit(connection::Attributes {
			height,
			connection_id: Some(connection_id),
			client_id: client_id.parse()?,
			counterparty_connection_id: if counterparty_connection_id.is_empty() {
				None
			} else {
				Some(counterparty_connection_id.parse()?)
			},
			counterparty_client_id: counterparty_client_id.parse()?,
		})))
	}
}

#[async_trait]
impl TryFromEvent<OpenTryConnectionFilter> for IbcEvent {
	async fn try_from_event(
		client: &EthereumClient,
		event: OpenTryConnectionFilter,
		log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		todo!("OpenTryConnectionFilter")
	}
}

#[async_trait]
impl TryFromEvent<OpenAckConnectionFilter> for IbcEvent {
	async fn try_from_event(
		client: &EthereumClient,
		event: OpenAckConnectionFilter,
		_log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		let OpenAckConnectionFilter { connection_id, counterparty_connection_id } = event;
		let connection_id: ConnectionId = connection_id.parse()?;
		let resp = client.query_connection_end(height, connection_id.clone()).await?;
		let connection_end = resp.connection.unwrap();
		let counterparty = connection_end.counterparty.unwrap();
		let client_id = connection_end.client_id.parse()?;
		let counterparty_client_id = counterparty.client_id.parse()?;
		Ok(IbcEvent::OpenAckConnection(connection::OpenAck(connection::Attributes {
			height,
			connection_id: Some(connection_id),
			client_id,
			counterparty_connection_id: if counterparty_connection_id.is_empty() {
				None
			} else {
				Some(counterparty_connection_id.parse()?)
			},
			counterparty_client_id,
		})))
	}
}

#[async_trait]
impl TryFromEvent<OpenInitChannelFilter> for IbcEvent {
	async fn try_from_event(
		client: &EthereumClient,
		event: OpenInitChannelFilter,
		_log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		let OpenInitChannelFilter { port_id, channel_id } = event;
		let port_id: PortId = port_id.parse()?;
		let channel_id: ChannelId = channel_id.parse()?;
		let resp = client.query_channel_end(height, channel_id, port_id.clone()).await?;
		let channel = resp.channel.unwrap();
		let counterparty = channel
			.counterparty
			.ok_or_else(|| ClientError::Other("counterparty not found".to_string()))?;
		Ok(IbcEvent::OpenInitChannel(channel::OpenInit {
			height,
			channel_id: Some(channel_id),
			counterparty_port_id: counterparty.port_id.parse()?,
			port_id,
			counterparty_channel_id: if counterparty.channel_id.is_empty() {
				None
			} else {
				Some(counterparty.channel_id.parse()?)
			},
			connection_id: channel.connection_hops[0].parse()?,
		}))
	}
}

#[async_trait]
impl TryFromEvent<OpenAckChannelFilter> for IbcEvent {
	async fn try_from_event(
		client: &EthereumClient,
		event: OpenAckChannelFilter,
		_log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		let OpenAckChannelFilter { port_id, channel_id } = event;
		let port_id: PortId = port_id.parse()?;
		let channel_id: ChannelId = channel_id.parse()?;
		let resp = client.query_channel_end(height, channel_id, port_id.clone()).await?;
		let channel = resp.channel.unwrap();
		let counterparty = channel.counterparty.unwrap();
		let counterparty_channel_id = counterparty.channel_id;
		Ok(IbcEvent::OpenAckChannel(channel::OpenAck {
			height,
			port_id,
			channel_id: Some(channel_id),
			counterparty_port_id: counterparty.port_id.parse()?,
			counterparty_channel_id: if counterparty_channel_id.is_empty() {
				None
			} else {
				Some(counterparty_channel_id.parse()?)
			},
			connection_id: channel.connection_hops[0].parse()?,
		}))
	}
}

#[async_trait]
impl TryFromEvent<SendPacketFilter> for IbcEvent {
	async fn try_from_event(
		client: &EthereumClient,
		event: SendPacketFilter,
		_log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		let SendPacketFilter {
			sequence,
			source_port_indexed: _,
			source_channel_indexed: _,
			source_port,
			source_channel,
			timeout_height,
			timeout_timestamp,
			data,
		} = event;
		let source_port: PortId = source_port.parse()?;
		let source_channel: ChannelId = source_channel.parse()?;
		let resp = client.query_channel_end(height, source_channel, source_port.clone()).await?;
		let channel = resp.channel.unwrap();
		let counterparty = channel.counterparty.unwrap();
		let counterparty_channel_id = counterparty.channel_id.parse()?;
		Ok(IbcEvent::SendPacket(channel::SendPacket {
			height,
			packet: Packet {
				sequence: Sequence::from(sequence),
				source_port,
				source_channel,
				destination_port: counterparty.port_id.parse()?,
				destination_channel: counterparty_channel_id,
				data: data.to_vec(),
				timeout_height: timeout_height.into(),
				timeout_timestamp: if timeout_timestamp == 0 {
					Timestamp::none()
				} else {
					Timestamp::from_nanoseconds(timeout_timestamp).expect("the timestamp is valid")
				},
			},
		}))
	}
}

#[async_trait]
impl TryFromEvent<WriteAcknowledgementFilter> for IbcEvent {
	async fn try_from_event(
		client: &EthereumClient,
		event: WriteAcknowledgementFilter,
		_log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		let WriteAcknowledgementFilter {
			sequence,
			destination_port,
			destination_channel,
			destination_port_indexed: _,
			destination_channel_indexed: _,
			acknowledgement,
		} = event;
		let destination_port_id: PortId = destination_port.parse()?;
		let destination_channel: ChannelId = destination_channel.parse()?;
		let packet = client
			.query_received_packets(
				height,
				destination_channel.clone(),
				destination_port_id.clone(),
				vec![sequence],
			)
			.await?
			.pop()
			.ok_or_else(|| ClientError::Other("packet not found".to_string()))?;
		log::info!(
			"ack = {}, ack' = {}",
			hex::encode(&acknowledgement),
			hex::encode(&packet.ack.unwrap_or_default())
		);
		Ok(IbcEvent::WriteAcknowledgement(channel::WriteAcknowledgement {
			height,
			packet: Packet {
				sequence: Sequence::from(sequence),
				source_port: packet.source_port.parse()?,
				source_channel: packet.source_channel.parse()?,
				destination_port: destination_port_id,
				destination_channel,
				data: packet.data,
				timeout_height: packet.timeout_height.into(),
				timeout_timestamp: if packet.timeout_timestamp == 0 {
					Timestamp::none()
				} else {
					Timestamp::from_nanoseconds(packet.timeout_timestamp)
						.expect("the timestamp is valid")
				},
			},
			ack: acknowledgement.to_vec(),
		}))
	}
}

#[async_trait]
impl TryFromEvent<AcknowledgePacketFilter> for IbcEvent {
	async fn try_from_event(
		_client: &EthereumClient,
		event: AcknowledgePacketFilter,
		_log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		let AcknowledgePacketFilter {
			packet:
				PacketData {
					sequence,
					source_port: source_port_raw,
					source_channel: source_channel_raw,
					destination_port: destination_port_raw,
					destination_channel: destination_channel_raw,
					data,
					timeout_height,
					timeout_timestamp,
				},
			acknowledgement: _,
		} = event;
		let source_port: PortId = source_port_raw.parse()?;
		let source_channel: ChannelId = source_channel_raw.parse()?;
		let destination_port: PortId = destination_port_raw.parse()?;
		let destination_channel: ChannelId = destination_channel_raw.parse()?;
		Ok(IbcEvent::AcknowledgePacket(channel::AcknowledgePacket {
			height,
			packet: Packet {
				sequence: Sequence::from(sequence),
				source_port,
				source_channel,
				destination_port,
				destination_channel,
				data: data.to_vec(),
				timeout_height: Height::new(
					timeout_height.revision_number,
					timeout_height.revision_height,
				),
				timeout_timestamp: Timestamp::from_nanoseconds(timeout_timestamp)
					.map_err(|_| ClientError::Other("invalid timeout height".to_string()))?,
			},
		}))
	}
}

#[async_trait]
impl TryFromEvent<TimeoutPacketFilter> for IbcEvent {
	async fn try_from_event(
		_client: &EthereumClient,
		event: TimeoutPacketFilter,
		_log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		let TimeoutPacketFilter {
			sequence,
			source_port: _,
			source_channel: _,
			source_port_raw,
			source_channel_raw,
			destination_port: destination_port_raw,
			destination_channel: destination_channel_raw,
			data,
			timeout_height,
			timeout_timestamp,
		} = event;
		let source_port: PortId = source_port_raw.parse()?;
		let source_channel: ChannelId = source_channel_raw.parse()?;
		let destination_port: PortId = destination_port_raw.parse()?;
		let destination_channel: ChannelId = destination_channel_raw.parse()?;
		Ok(IbcEvent::TimeoutPacket(channel::TimeoutPacket {
			height,
			packet: Packet {
				sequence: Sequence::from(sequence),
				source_port,
				source_channel,
				destination_port,
				destination_channel,
				data: data.to_vec(),
				timeout_height: Height::new(
					timeout_height.revision_number,
					timeout_height.revision_height,
				),
				timeout_timestamp: Timestamp::from_nanoseconds(timeout_timestamp)
					.map_err(|_| ClientError::Other("invalid timeout height".to_string()))?,
			},
		}))
	}
}

#[async_trait]
impl TryFromEvent<TimeoutOnClosePacketFilter> for IbcEvent {
	async fn try_from_event(
		_client: &EthereumClient,
		event: TimeoutOnClosePacketFilter,
		_log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		let TimeoutOnClosePacketFilter {
			sequence,
			source_port: _,
			source_channel: _,
			source_port_raw,
			source_channel_raw,
			destination_port: destination_port_raw,
			destination_channel: destination_channel_raw,
			data,
			timeout_height,
			timeout_timestamp,
		} = event;
		let source_port: PortId = source_port_raw.parse()?;
		let source_channel: ChannelId = source_channel_raw.parse()?;
		let destination_port: PortId = destination_port_raw.parse()?;
		let destination_channel: ChannelId = destination_channel_raw.parse()?;
		Ok(IbcEvent::TimeoutOnClosePacket(channel::TimeoutOnClosePacket {
			height,
			packet: Packet {
				sequence: Sequence::from(sequence),
				source_port,
				source_channel,
				destination_port,
				destination_channel,
				data: data.to_vec(),
				timeout_height: Height::new(
					timeout_height.revision_number,
					timeout_height.revision_height,
				),
				timeout_timestamp: Timestamp::from_nanoseconds(timeout_timestamp)
					.map_err(|_| ClientError::Other("invalid timeout height".to_string()))?,
			},
		}))
	}
}

#[async_trait]
impl TryFromEvent<CloseInitChannelFilter> for IbcEvent {
	async fn try_from_event(
		client: &EthereumClient,
		event: CloseInitChannelFilter,
		_log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		let CloseInitChannelFilter { port_id, channel_id } = event;
		let port_id: PortId = port_id.parse()?;
		let channel_id: ChannelId = channel_id.parse()?;
		let channel = client
			.query_channel_end(height, channel_id, port_id.clone())
			.await?
			.channel
			.unwrap();
		let counterparty = channel
			.counterparty
			.ok_or_else(|| ClientError::Other("counterparty not found".to_string()))?;
		Ok(IbcEvent::CloseInitChannel(channel::CloseInit {
			height,
			port_id,
			channel_id,
			connection_id: channel.connection_hops[0].parse()?,
			counterparty_port_id: counterparty.port_id.parse()?,
			counterparty_channel_id: if counterparty.channel_id.is_empty() {
				None
			} else {
				Some(counterparty.channel_id.parse()?)
			},
		}))
	}
}

#[async_trait]
impl TryFromEvent<CloseConfirmChannelFilter> for IbcEvent {
	async fn try_from_event(
		client: &EthereumClient,
		event: CloseConfirmChannelFilter,
		_log: Log,
		height: Height,
	) -> Result<Self, ClientError> {
		let CloseConfirmChannelFilter { port_id, channel_id } = event;
		let port_id: PortId = port_id.parse()?;
		let channel_id: ChannelId = channel_id.parse()?;
		let channel = client
			.query_channel_end(height, channel_id, port_id.clone())
			.await?
			.channel
			.unwrap();
		let counterparty = channel
			.counterparty
			.ok_or_else(|| ClientError::Other("counterparty not found".to_string()))?;
		Ok(IbcEvent::CloseConfirmChannel(CloseConfirm {
			height,
			port_id,
			connection_id: channel.connection_hops[0].parse()?,
			counterparty_port_id: counterparty.port_id.parse()?,
			channel_id: Some(channel_id),
			counterparty_channel_id: if counterparty.channel_id.is_empty() {
				None
			} else {
				Some(counterparty.channel_id.parse()?)
			},
		}))
	}
}