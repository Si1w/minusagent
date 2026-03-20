pub struct Context {
    pub system_prompt: String,
    pub history: Vec<Message>,
}

pub struct Message {
    pub role: Role,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
}

pub enum Role {
    User,
    Assistant,
    Tool,
}

pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

pub struct LlmConfig {
    pub model: String,
    pub base_url: String,
    pub api_key: String,
}

pub struct Config {
    pub llm: LlmConfig,
}

pub struct SystemState {
    pub config: Config,
}

pub struct SharedStore {
    pub context: Context,
    pub state: SystemState,
}
