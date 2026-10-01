//! Talking to Ollama. With `setup`'s model download, this is all of heyListen's network code
//! (see docs/adr/0004).

use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize)]
struct Tags {
    models: Vec<Model>,
}

#[derive(Deserialize)]
struct Model {
    name: String,
}

/// Names of the models Ollama has pulled.
pub fn list_models(url: &str) -> Result<Vec<String>> {
    let tags: Tags = agent(Duration::from_secs(3)).get(format!("{}/api/tags", url.trim_end_matches('/'))).call()?.body_mut().read_json()?;
    Ok(tags.models.into_iter().map(|m| m.name).collect())
}

/// Ollama reports `name` as `name:latest` when no tag was given.
pub fn has_model(models: &[String], wanted: &str) -> bool {
    models.iter().any(|m| m == wanted || m.strip_suffix(":latest") == Some(wanted))
}

/// The model's context window in tokens, if Ollama reports one.
pub fn context_length(url: &str, model: &str) -> Result<Option<u64>> {
    #[derive(Deserialize)]
    struct Show {
        model_info: std::collections::HashMap<String, serde_json::Value>,
    }
    let show: Show = agent(Duration::from_secs(10))
        .post(format!("{}/api/show", url.trim_end_matches('/')))
        .send_json(serde_json::json!({ "model": model }))?
        .body_mut()
        .read_json()?;
    Ok(show.model_info.iter().find(|(k, _)| k.ends_with(".context_length")).and_then(|(_, v)| v.as_u64()))
}

/// One non-streaming chat turn: system prompt + user message → reply.
pub fn chat(url: &str, model: &str, num_ctx: u64, system: &str, user: &str, json: bool) -> Result<String> {
    #[derive(Deserialize)]
    struct Reply {
        message: Message,
    }
    #[derive(Deserialize)]
    struct Message {
        content: String,
    }
    let mut body = serde_json::json!({
        "model": model,
        "stream": false,
        "options": { "num_ctx": num_ctx, "temperature": 0.2 },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    });
    if json {
        body["format"] = serde_json::json!("json");
    }
    // A long meeting on a 12B model can take minutes.
    let reply: Reply = agent(Duration::from_secs(30 * 60))
        .post(format!("{}/api/chat", url.trim_end_matches('/')))
        .send_json(body)?
        .body_mut()
        .read_json()?;
    Ok(reply.message.content.trim().to_string())
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).build().into()
}

/// Pulls a model, calling `progress(status, completed_bytes, total_bytes)` as it goes.
pub fn pull(url: &str, model: &str, mut progress: impl FnMut(&str, u64, u64)) -> Result<()> {
    use std::io::BufRead;
    #[derive(Deserialize)]
    struct Line {
        #[serde(default)]
        status: String,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        total: u64,
        #[serde(default)]
        completed: u64,
    }
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(None).build().into();
    let mut response = agent
        .post(format!("{}/api/pull", url.trim_end_matches('/')))
        .send_json(serde_json::json!({ "model": model, "stream": true }))?;
    for line in std::io::BufReader::new(response.body_mut().as_reader()).lines() {
        let line: Line = serde_json::from_str(&line?)?;
        if let Some(e) = line.error {
            anyhow::bail!("Ollama couldn't pull {model}: {e}");
        }
        progress(&line.status, line.completed, line.total);
        if line.status == "success" {
            return Ok(());
        }
    }
    anyhow::bail!("Ollama stopped pulling {model} before it finished")
}
