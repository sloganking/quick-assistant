use reqwest::Client;
use serde_json::json;
use std::{error::Error, time::Duration};
use tokio::time::sleep;

pub async fn search_web(api_key: &str, query: &str) -> Result<String, Box<dyn Error>> {
    let client = Client::new();
    let base = "https://api.openai.com/v1";

    let assistant_res: serde_json::Value = client
        .post(&format!("{}/assistants", base))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("OpenAI-Beta", "assistants=v1")
        .json(&json!({
            "model": "gpt-4o",
            "instructions": "You are a web search assistant.",
            "tools": [{"type": "browser"}]
        }))
        .send()
        .await?
        .json()
        .await?;

    let assistant_id = assistant_res["id"]
        .as_str()
        .ok_or("missing assistant id")?
        .to_string();

    let thread_res: serde_json::Value = client
        .post(&format!("{}/threads", base))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("OpenAI-Beta", "assistants=v1")
        .send()
        .await?
        .json()
        .await?;

    let thread_id = thread_res["id"]
        .as_str()
        .ok_or("missing thread id")?
        .to_string();

    client
        .post(&format!("{}/threads/{}/messages", base, thread_id))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("OpenAI-Beta", "assistants=v1")
        .json(&json!({"role": "user", "content": query}))
        .send()
        .await?;

    let run_res: serde_json::Value = client
        .post(&format!("{}/threads/{}/runs", base, thread_id))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("OpenAI-Beta", "assistants=v1")
        .json(&json!({"assistant_id": assistant_id}))
        .send()
        .await?
        .json()
        .await?;

    let run_id = run_res["id"].as_str().ok_or("missing run id")?.to_string();

    loop {
        let run_status: serde_json::Value = client
            .get(&format!("{}/threads/{}/runs/{}", base, thread_id, run_id))
            .header("Authorization", format!("Bearer {}", api_key))
            .header("OpenAI-Beta", "assistants=v1")
            .send()
            .await?
            .json()
            .await?;

        match run_status["status"].as_str() {
            Some("completed") => break,
            Some("failed") | Some("expired") | Some("cancelled") => return Err("run failed".into()),
            _ => sleep(Duration::from_secs(1)).await,
        }
    }

    let messages: serde_json::Value = client
        .get(&format!("{}/threads/{}/messages", base, thread_id))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("OpenAI-Beta", "assistants=v1")
        .send()
        .await?
        .json()
        .await?;

    let answer = messages["data"][0]["content"][0]["text"]["value"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // cleanup
    let _ = client
        .delete(&format!("{}/assistants/{}", base, assistant_id))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("OpenAI-Beta", "assistants=v1")
        .send()
        .await;
    let _ = client
        .delete(&format!("{}/threads/{}", base, thread_id))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("OpenAI-Beta", "assistants=v1")
        .send()
        .await;

    Ok(answer)
}
