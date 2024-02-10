use anyhow::{anyhow, Error};
use std::{
	thread,
	time::{Duration, SystemTime},
};
use ureq;

#[derive(Debug, Clone)]
pub struct ZKProver {
	pub prover_url: String,
	pub delay_secs: u64
}

#[derive(Debug, serde::Deserialize)]
pub struct Response {
	pub proof_id: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct ResponseProofRequest {
	pub status: String,
	pub proof: Option<Vec<u8>>,
}

#[derive(Debug, serde::Serialize)]
pub struct CreateProofInput {
	pub signatures: Vec<Vec<u8>>,
	pub msgs: Vec<Vec<u8>>,
	pub public_keys: Vec<Vec<u8>>,
}

impl CreateProofInput{
	pub fn new(signatures: Vec<Vec<u8>>, msgs: Vec<Vec<u8>>, public_keys: Vec<Vec<u8>>) -> Self {
		Self { signatures, msgs, public_keys }
	}
}

impl ZKProver {
	pub fn new(prover_url: String, delay_secs: u64) -> Self {
		Self { prover_url, delay_secs: delay_secs }
	}

    pub fn status(&self) -> Result<String, Error> {
        let url = format!("{}{}", self.prover_url, "/status");
		let result= ureq::get(url.as_str())
            .call()?
            .into_string()?;
        println!("result: {:?}", result);
        Ok(result)
	}



	pub fn create_proof(&self, proof_input: CreateProofInput) -> Result<Response, Error> {
        let url = format!("{}{}", self.prover_url, "/create_proof");
		let result= ureq::post(url.as_str())
			.send_json(ureq::json!(proof_input))?
			.into_string();
        println!("result: {:?}", result);
        match result {
            Ok(r) => {
                let resp: Response = serde_json::from_str(&r)?;
                Ok(resp)
            },
            Err(e) => {
                Err(anyhow!("Error: {:?}", e))
            }
        }
	}

	pub fn poll_proof(&self, proof_id: &str) -> Result<Option<Vec<u8>>, Error> {
		let url = self.prover_url.clone() + "/get_proof";
		let resp = ureq::post(url.as_str())
			.send_json(ureq::json!({
				"proof_id": proof_id
			}))?
			.into_string()?;

        let resp: ResponseProofRequest = serde_json::from_str(&resp)?;
        println!("resp: {:?}", resp);

		// TOOD: handle some edge cases
		match resp.status.as_str() {
			"COMPLETED" => Ok(resp.proof),
			_ => Ok(None),
		}
	}
}