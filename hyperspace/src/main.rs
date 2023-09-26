//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use anyhow::{bail, Result};
use clap::Parser;
use hyperspace_core::{
	chain::{AnyCmd, AnyConfig},
	command::{Cli, Subcommand},
	logging,
};

#[tokio::main]
async fn main() -> Result<()> {
	logging::setup_logging();
	let cli = Cli::parse();

	match &cli.subcommand {
		Subcommand::Relay(cmd) => cmd.run().await,
		Subcommand::UploadWasm(cmd) => {
			let new_config = cmd.run().await?;
			cmd.save_config(&new_config).await
		},
		Subcommand::CreateClients(cmd) => {
			let new_config = cmd.create_clients().await?;
			cmd.save_config(&new_config).await
		},
		Subcommand::CreateConnection(cmd) => {
			let new_config = cmd.create_connection().await?;
			cmd.save_config(&new_config).await
		},
		Subcommand::CreateChannel(cmd) => {
			let new_config = cmd.create_channel().await?;
			cmd.save_config(&new_config).await
		},
		Subcommand::Fish(cmd) => cmd.fish().await,
		Subcommand::Client { cmd, client } => {
			let mut config = cmd.parse_config().await?;
			match (client, config.chain_a) {
				(AnyCmd::Ethereum(client_cmd), AnyConfig::Ethereum(client_cfg)) => {
					let new_config = client_cmd.run(client_cfg).await?;
					config.chain_a = AnyConfig::Ethereum(new_config);
					cmd.save_config(&config).await
				},
				(cmd, cfg) =>
					bail!("Client command {:?} is not supported for chain {:?}", cmd, cfg),
			}
		},
	}
}
