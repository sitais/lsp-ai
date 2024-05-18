use anyhow::Context;
use hf_hub::api::sync::ApiBuilder;
use serde::Deserialize;
use serde_json::Value;
use tracing::instrument;

use crate::{
    config::{self, ChatMessage, FIM},
    memory_backends::Prompt,
    template::apply_chat_template,
    transformer_worker::{
        DoCompletionResponse, DoGenerationResponse, DoGenerationStreamResponse,
        GenerationStreamRequest,
    },
    utils::format_chat_messages,
};

mod model;
use model::Model;

use super::TransformerBackend;

const fn max_new_tokens_default() -> usize {
    32
}

#[derive(Debug, Deserialize)]
pub struct LLaMACPPRunParams {
    pub fim: Option<FIM>,
    messages: Option<Vec<ChatMessage>>,
    chat_template: Option<String>, // A Jinja template
    chat_format: Option<String>,   // The name of a template in llamacpp
    #[serde(default = "max_new_tokens_default")]
    pub max_new_tokens: usize,
    // TODO: Explore other arguments
}

pub struct LLaMACPP {
    model: Model,
}

impl LLaMACPP {
    #[instrument]
    pub fn new(configuration: config::LLaMACPP) -> anyhow::Result<Self> {
        let api = ApiBuilder::new().with_progress(true).build()?;
        let name = configuration
            .model
            .name
            .as_ref()
            .context("Please set `name` to use LLaMA.cpp")?;
        let repo = api.model(configuration.model.repository.to_owned());
        let model_path = repo.get(name)?;
        let model = Model::new(model_path, &configuration)?;
        Ok(Self { model })
    }

    #[instrument(skip(self))]
    fn get_prompt_string(
        &self,
        prompt: &Prompt,
        params: &LLaMACPPRunParams,
    ) -> anyhow::Result<String> {
        Ok(match &params.messages {
            Some(completion_messages) => {
                let chat_messages = format_chat_messages(completion_messages, prompt);
                if let Some(chat_template) = &params.chat_template {
                    let bos_token = self.model.get_bos_token()?;
                    let eos_token = self.model.get_eos_token()?;
                    apply_chat_template(chat_template, chat_messages, &bos_token, &eos_token)?
                } else {
                    self.model
                        .apply_chat_template(chat_messages, params.chat_format.clone())?
                }
            }
            None => prompt.code.to_owned(),
        })
    }
}

#[async_trait::async_trait]
impl TransformerBackend for LLaMACPP {
    #[instrument(skip(self))]
    async fn do_completion(
        &self,
        prompt: &Prompt,
        params: Value,
    ) -> anyhow::Result<DoCompletionResponse> {
        let params: LLaMACPPRunParams = serde_json::from_value(params)?;
        let prompt = self.get_prompt_string(prompt, &params)?;
        self.model
            .complete(&prompt, params)
            .map(|insert_text| DoCompletionResponse { insert_text })
    }

    #[instrument(skip(self))]
    async fn do_generate(
        &self,
        prompt: &Prompt,
        params: Value,
    ) -> anyhow::Result<DoGenerationResponse> {
        let params: LLaMACPPRunParams = serde_json::from_value(params)?;
        let prompt = self.get_prompt_string(prompt, &params)?;
        self.model
            .complete(&prompt, params)
            .map(|generated_text| DoGenerationResponse { generated_text })
    }

    #[instrument(skip(self))]
    async fn do_generate_stream(
        &self,
        _request: &GenerationStreamRequest,
        _params: Value,
    ) -> anyhow::Result<DoGenerationStreamResponse> {
        anyhow::bail!("GenerationStream is not yet implemented")
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use serde_json::json;

    // // "completion": [
    // //     {
    // //         "role": "system",
    // //         "content": "You are a code completion chatbot. Use the following context to complete the next segement of code. Keep your response brief. Do not produce any text besides code. \n\n{context}",
    // //     },
    // //     {
    // //         "role": "user",
    // //         "content": "Complete the following code: \n\n{code}"
    // //     }
    // // ],
    // // "generation": [
    // //     {
    // //         "role": "system",
    // //         "content": "You are a code completion chatbot. Use the following context to complete the next segement of code. \n\n{context}",
    // //     },
    // //     {
    // //         "role": "user",
    // //         "content": "Complete the following code: \n\n{code}"
    // //     }
    // // ],
    // "chat_template": "{% if not add_generation_prompt is defined %}\n{% set add_generation_prompt = false %}\n{% endif %}\n{%- set ns = namespace(found=false) -%}\n{%- for message in messages -%}\n    {%- if message['role'] == 'system' -%}\n        {%- set ns.found = true -%}\n    {%- endif -%}\n{%- endfor -%}\n{{bos_token}}{%- if not ns.found -%}\n{{'You are an AI programming assistant, utilizing the Deepseek Coder model, developed by Deepseek Company, and you only answer questions related to computer science. For politically sensitive questions, security and privacy issues, and other non-computer science questions, you will refuse to answer\\n'}}\n{%- endif %}\n{%- for message in messages %}\n    {%- if message['role'] == 'system' %}\n{{ message['content'] }}\n    {%- else %}\n        {%- if message['role'] == 'user' %}\n{{'### Instruction:\\n' + message['content'] + '\\n'}}\n        {%- else %}\n{{'### Response:\\n' + message['content'] + '\\n<|EOT|>\\n'}}\n        {%- endif %}\n    {%- endif %}\n{%- endfor %}\n{% if add_generation_prompt %}\n{{'### Response:'}}\n{% endif %}"

    #[tokio::test]
    async fn llama_cpp_do_completion() -> anyhow::Result<()> {
        let configuration: config::LLaMACPP = serde_json::from_value(json!({
            "repository": "stabilityai/stable-code-3b",
            "name": "stable-code-3b-Q5_K_M.gguf",
            "n_ctx": 2048,
            "n_gpu_layers": 35,
        }))?;
        let llama_cpp = LLaMACPP::new(configuration).unwrap();
        let prompt = Prompt::default_with_cursor();
        let run_params = json!({
            "fim": {
                "start": "<fim_prefix>",
                "middle": "<fim_suffix>",
                "end": "<fim_middle>"
            },
            "max_tokens": 64
        });
        let response = llama_cpp.do_completion(&prompt, run_params).await?;
        assert!(!response.insert_text.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn llama_cpp_do_generate() -> anyhow::Result<()> {
        let configuration: config::LLaMACPP = serde_json::from_value(json!({
            "repository": "stabilityai/stable-code-3b",
            "name": "stable-code-3b-Q5_K_M.gguf",
            "n_ctx": 2048,
            "n_gpu_layers": 35,
        }))?;
        let llama_cpp = LLaMACPP::new(configuration).unwrap();
        let prompt = Prompt::default_with_cursor();
        let run_params = json!({
            "fim": {
                "start": "<fim_prefix>",
                "middle": "<fim_suffix>",
                "end": "<fim_middle>"
            },
            "max_tokens": 64
        });
        let response = llama_cpp.do_generate(&prompt, run_params).await?;
        assert!(!response.generated_text.is_empty());
        Ok(())
    }
}
