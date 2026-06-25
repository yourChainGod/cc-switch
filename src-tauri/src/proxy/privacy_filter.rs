//! 隐私过滤代理集成（plumbing）
//!
//! 在请求转发前，对最终上游请求体中的文本字段做脱敏（mask）。
//! 复用 [`crate::privacy_filter`] 的进程内检测引擎，按各家 API 的请求结构
//! 就地（in-place）改写文本，不做提取/回填往返，避免路径错配。
//!
//! best-effort：仅改写字符串值、不动结构，规范化顺序得以保持；任何字段缺失都安全跳过。
//!
//! 覆盖字段（探测 key，与 app_type 无关，兼容 claude-as-openai 等混合形态）：
//! - Claude Messages：`system`、`messages[].content`（含 `tool_result` 嵌套 `content`）
//! - OpenAI Chat：`messages[].content`（string 或 `{type,text}` 块数组）
//! - OpenAI Responses（Codex）：`instructions`、`input`（含 `function_call_output.output`）
//! - Gemini：`systemInstruction`、`contents[].parts[].text`

use crate::privacy_filter::redact_text;
use crate::proxy::types::PrivacyFilterConfig;
use serde_json::Value;

/// 对最终上游请求体执行脱敏，返回命中的敏感片段总数（0 表示无改动）。
pub fn apply(body: &mut Value, cfg: &PrivacyFilterConfig) -> usize {
    let mut count = 0usize;
    let obj = match body.as_object_mut() {
        Some(o) => o,
        None => return 0,
    };

    // Claude system：string 或 块数组
    if let Some(system) = obj.get_mut("system") {
        redact_content(system, cfg, &mut count);
    }

    // OpenAI Responses instructions：string
    if let Some(instructions) = obj.get_mut("instructions") {
        redact_string(instructions, cfg, &mut count);
    }

    // Claude / OpenAI Chat messages[].content
    if let Some(Value::Array(messages)) = obj.get_mut("messages") {
        for msg in messages.iter_mut() {
            if let Some(content) = msg.get_mut("content") {
                redact_content(content, cfg, &mut count);
            }
        }
    }

    // OpenAI Responses input：string 或 元素数组（message / function_call_output）
    if let Some(input) = obj.get_mut("input") {
        match input {
            Value::String(_) => redact_string(input, cfg, &mut count),
            Value::Array(items) => {
                for item in items.iter_mut() {
                    redact_block(item, cfg, &mut count);
                }
            }
            _ => {}
        }
    }

    // Gemini systemInstruction：{parts:[{text}]} 或 string
    if let Some(si) = obj.get_mut("systemInstruction") {
        redact_parts_holder(si, cfg, &mut count);
    }

    // Gemini contents[].parts[].text
    if let Some(Value::Array(contents)) = obj.get_mut("contents") {
        for c in contents.iter_mut() {
            redact_parts_holder(c, cfg, &mut count);
        }
    }

    count
}

/// 就地脱敏一个字符串值。
fn redact_string(v: &mut Value, cfg: &PrivacyFilterConfig, count: &mut usize) {
    if let Value::String(s) = v {
        let outcome = redact_text(s, cfg);
        if outcome.count > 0 {
            *s = outcome.redacted;
            *count += outcome.count;
        }
    }
}

/// 脱敏“content”值：可能是字符串，或内容块数组（Claude/OpenAI 通用）。
fn redact_content(v: &mut Value, cfg: &PrivacyFilterConfig, count: &mut usize) {
    match v {
        Value::String(_) => redact_string(v, cfg, count),
        Value::Array(blocks) => {
            for b in blocks.iter_mut() {
                redact_block(b, cfg, count);
            }
        }
        _ => {}
    }
}

/// 脱敏一个内容块：覆盖 `text`（文本块/part）、`content`（tool_result 嵌套）、`output`（responses 工具输出）。
fn redact_block(block: &mut Value, cfg: &PrivacyFilterConfig, count: &mut usize) {
    match block {
        Value::String(_) => redact_string(block, cfg, count),
        Value::Object(obj) => {
            if let Some(text) = obj.get_mut("text") {
                redact_string(text, cfg, count);
            }
            if let Some(content) = obj.get_mut("content") {
                redact_content(content, cfg, count);
            }
            if let Some(output) = obj.get_mut("output") {
                redact_content(output, cfg, count);
            }
        }
        _ => {}
    }
}

/// 脱敏 Gemini 的 `{parts:[{text}]}` 容器（或退化为 string）。
fn redact_parts_holder(v: &mut Value, cfg: &PrivacyFilterConfig, count: &mut usize) {
    match v {
        Value::String(_) => redact_string(v, cfg, count),
        Value::Object(obj) => {
            if let Some(Value::Array(parts)) = obj.get_mut("parts") {
                for p in parts.iter_mut() {
                    if let Some(text) = p.get_mut("text") {
                        redact_string(text, cfg, count);
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg() -> PrivacyFilterConfig {
        PrivacyFilterConfig {
            enabled: true,
            email: true,
            phone: true,
            id_card: true,
            bank_card: true,
            ip: true,
            secret: true,
        }
    }

    #[test]
    fn claude_system_and_messages() {
        let mut body = json!({
            "model": "claude-3",
            "system": "联系 admin@corp.com",
            "messages": [
                {"role": "user", "content": "我的手机号 13800138000"},
                {"role": "user", "content": [
                    {"type": "text", "text": "邮箱 a@b.com"},
                    {"type": "tool_result", "content": [{"type": "text", "text": "另一个 c@d.com"}]}
                ]}
            ]
        });
        let n = apply(&mut body, &cfg());
        assert_eq!(n, 4);
        assert_eq!(body["system"], json!("联系 [邮箱]"));
        assert_eq!(body["messages"][0]["content"], json!("我的手机号 [电话]"));
        assert_eq!(
            body["messages"][1]["content"][0]["text"],
            json!("邮箱 [邮箱]")
        );
        assert_eq!(
            body["messages"][1]["content"][1]["content"][0]["text"],
            json!("另一个 [邮箱]")
        );
        // 结构与非文本字段保持不变
        assert_eq!(body["model"], json!("claude-3"));
    }

    #[test]
    fn responses_instructions_and_input() {
        let mut body = json!({
            "instructions": "用户邮箱 x@y.com",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "手机 13912345678"}]},
                {"type": "function_call_output", "output": "结果里有 z@w.com"}
            ]
        });
        let n = apply(&mut body, &cfg());
        assert_eq!(n, 3);
        assert_eq!(body["instructions"], json!("用户邮箱 [邮箱]"));
        assert_eq!(body["input"][0]["content"][0]["text"], json!("手机 [电话]"));
        assert_eq!(body["input"][1]["output"], json!("结果里有 [邮箱]"));
    }

    #[test]
    fn gemini_contents() {
        let mut body = json!({
            "systemInstruction": {"parts": [{"text": "system 邮箱 s@t.com"}]},
            "contents": [
                {"role": "user", "parts": [{"text": "我的邮箱 u@v.com"}, {"text": "无敏感"}]}
            ]
        });
        let n = apply(&mut body, &cfg());
        assert_eq!(n, 2);
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            json!("system 邮箱 [邮箱]")
        );
        assert_eq!(
            body["contents"][0]["parts"][0]["text"],
            json!("我的邮箱 [邮箱]")
        );
        assert_eq!(body["contents"][0]["parts"][1]["text"], json!("无敏感"));
    }

    #[test]
    fn no_hits_returns_zero_and_unchanged() {
        let mut body = json!({"messages": [{"role": "user", "content": "普通问题，无敏感信息"}]});
        let before = body.clone();
        let n = apply(&mut body, &cfg());
        assert_eq!(n, 0);
        assert_eq!(body, before);
    }
}
