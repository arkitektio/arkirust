use super::fakts_protocol::{
    DeviceCodeAnswer, DeviceCodeChallengeAnswer, DeviceCodeChallengeRequest,
    DeviceCodeStartRequest, FaktsAnswer, Manifest, Requirement, RetrieveRequest, TokenConfig,
};

pub async fn get_saved_token() -> Result<Option<String>, Box<dyn std::error::Error>> {
    // read save token form token.json
    let token_path = std::path::Path::new("token.json");
    // parse token from json according to a struct with token field
    if !token_path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(token_path)?;
    let token_data: TokenConfig = serde_json::from_str(&contents)?;
    Ok(Some(token_data.token))
}

pub async fn claim_fakts<T: ::serde::de::DeserializeOwned + std::fmt::Debug>(
    token: String,
) -> Result<T, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let retrieve_response = client
        .post("http://127.0.0.1/lok/f/claim/")
        .json(&RetrieveRequest {
            token: token.clone(),
        })
        .send()
        .await?;

    let body = retrieve_response.text().await?;

    let fakts_answer: FaktsAnswer<T> = match serde_json::from_str(&body) {
        Ok(answer) => answer,
        Err(e) => {
            println!("Failed to deserialize response: {}", body);
            return Err(Box::new(e));
        }
    };

    println!("Response from register_client: {:?}", fakts_answer);
    return Ok(fakts_answer.config);
}

pub async fn register_client<T: ::serde::de::DeserializeOwned + std::fmt::Debug>(
    manifest: Manifest,
) -> Result<T, Box<dyn std::error::Error>> {
    // Try to retrive from saved token (if any)

    let token = get_saved_token().await?;
    if let Some(token) = token {
        match claim_fakts(token).await {
            Ok(fakts) => return Ok(fakts),
            Err(_) => (), // Continue with rest of function if error occurs
        };
    }

    let request = DeviceCodeStartRequest {
        manifest: manifest,
        requested_client_kind: "development".to_string(),
    };

    let client = reqwest::Client::new();
    let res = client
        .post("http://127.0.0.1/lok/f/start/")
        .json(&request)
        .send()
        .await?;

    let body = res.text().await?;

    // Parse the response body into a DeviceCodeAnswer struct
    let device_code_answer: DeviceCodeAnswer = serde_json::from_str(&body)?;

    println!("Response from register_client: http://127.0.0.1/lok/f/configure/?grant=device_code&device_code={}", device_code_answer.code);

    // Check if the challenge has been accepted for a while
    // and if the status is still pending

    let mut counter = 0;

    let challenge = DeviceCodeChallengeRequest {
        code: device_code_answer.code,
    };

    let mut token: Option<String> = None;

    loop {
        let res = client
            .post("http://127.0.0.1/lok/f/challenge/")
            .json(&challenge)
            .send()
            .await?;

        let body = res.text().await?;

        let device_code_answer: DeviceCodeChallengeAnswer = match serde_json::from_str(&body) {
            Ok(answer) => answer,
            Err(e) => {
                println!("Failed to deserialize response: {}", body);
                return Err(Box::new(e));
            }
        };

        if device_code_answer.status == "granted" {
            token = Some(device_code_answer.token.unwrap());
            break;
        }

        // Sleep for a while
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        counter += 1;

        if counter > 10 {
            break;
        }
    }

    // Check if token is None
    let token = match token {
        Some(token) => token,
        None => panic!("Token is None"),
    };

    // Save token to token.json
    let token_path = std::path::Path::new("token.json");

    let token_data = TokenConfig {
        token: token.clone(),
    };
    let token_json = serde_json::to_string(&token_data)?;

    std::fs::write(token_path, token_json)?;

    Ok(claim_fakts(token).await?)
}
