use crate::config::AgentPaths;
use anyhow::Result;
use chrono::{Local, Utc};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub fn append_global(paths: &AgentPaths, content: &str, tags: &[String]) -> Result<String> {
    let ts = Utc::now();
    let id = format!("mem_{}", ts.format("%Y%m%d%H%M%S"));
    let tags_line = if tags.is_empty() {
        "none".to_string()
    } else {
        tags.join(", ")
    };

    let entry = format!(
        "## {id}\n\
timestamp: {}\n\
tags: {tags_line}\n\
content:\n\
{content}\n\
\n\
---\n\n",
        ts.to_rfc3339()
    );

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.memory_file)?;
    file.write_all(entry.as_bytes())?;
    Ok(id)
}

pub fn tail_context(paths: &AgentPaths, max_chars: usize) -> Result<String> {
    let global = fs::read_to_string(&paths.memory_file).unwrap_or_default();
    let mut merged = String::new();
    merged.push_str("## Long-Term Memory (tail)\n");
    merged.push_str(&take_tail_chars(
        &strip_assistant_sections(&global),
        max_chars / 2,
    ));
    merged.push_str("\n\n## Recent Short-Term Memory\n");

    let mut short_term_files = list_short_term_files(&paths.memory_dir)?;
    short_term_files.sort();
    short_term_files.reverse();

    for file in short_term_files.into_iter().take(7) {
        let content = fs::read_to_string(file).unwrap_or_default();
        merged.push_str(&take_tail_chars(
            &strip_assistant_sections(&content),
            max_chars / 8,
        ));
        merged.push('\n');
    }

    Ok(take_tail_chars(&merged, max_chars))
}

pub fn append_short_term(paths: &AgentPaths, source: &str, content: &str) -> Result<()> {
    let now = Local::now();
    let filename = format!("{}.md", now.format("%Y-%m-%d"));
    let file_path = paths.memory_dir.join(filename);

    let block = format!(
        "## {}\nsource: {source}\ncontent:\n{content}\n\n",
        now.to_rfc3339()
    );

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)?;
    file.write_all(block.as_bytes())?;
    Ok(())
}

pub fn auto_capture_long_term(
    paths: &AgentPaths,
    source: &str,
    user_input: &str,
) -> Result<Vec<String>> {
    let mut memory_index =
        normalize_for_compare(&fs::read_to_string(&paths.memory_file).unwrap_or_default());
    let mut seen = HashSet::new();
    let mut added = Vec::new();

    for candidate in extract_memory_candidates(user_input) {
        let tags = vec![
            "auto".to_string(),
            source.to_string(),
            infer_memory_tag(&candidate).to_string(),
        ];
        try_capture_candidate(
            paths,
            &mut memory_index,
            &mut seen,
            &mut added,
            candidate,
            tags,
        )?;
    }

    for sentence in split_sentences(user_input) {
        if !is_repeat_candidate(&sentence) {
            continue;
        }
        let count = count_short_term_occurrences(paths, &sentence)?;
        if count >= 3 {
            let tags = vec![
                "auto".to_string(),
                source.to_string(),
                "repeated".to_string(),
            ];
            try_capture_candidate(
                paths,
                &mut memory_index,
                &mut seen,
                &mut added,
                sentence,
                tags,
            )?;
        }
    }

    Ok(added)
}

pub fn auto_capture_event(paths: &AgentPaths, source: &str, event_text: &str) -> Result<bool> {
    let mut memory_index =
        normalize_for_compare(&fs::read_to_string(&paths.memory_file).unwrap_or_default());
    let mut seen = HashSet::new();
    let mut added = Vec::new();

    let tags = vec!["auto".to_string(), source.to_string(), "event".to_string()];
    try_capture_candidate(
        paths,
        &mut memory_index,
        &mut seen,
        &mut added,
        event_text.trim().to_string(),
        tags,
    )?;

    Ok(!added.is_empty())
}

pub fn capture_explicit_remember(
    paths: &AgentPaths,
    source: &str,
    text: &str,
) -> Result<Vec<String>> {
    let mut memory_index =
        normalize_for_compare(&fs::read_to_string(&paths.memory_file).unwrap_or_default());
    let mut seen = HashSet::new();
    let mut added = Vec::new();

    for sentence in split_sentences(text) {
        if !is_explicit_remember_sentence(&sentence) {
            continue;
        }
        let tags = vec![
            "auto".to_string(),
            source.to_string(),
            "explicit-remember".to_string(),
        ];
        try_capture_candidate(
            paths,
            &mut memory_index,
            &mut seen,
            &mut added,
            sentence,
            tags,
        )?;
    }
    Ok(added)
}

fn try_capture_candidate(
    paths: &AgentPaths,
    memory_index: &mut String,
    seen: &mut HashSet<String>,
    added: &mut Vec<String>,
    candidate: String,
    tags: Vec<String>,
) -> Result<()> {
    let normalized = normalize_for_compare(&candidate);
    if normalized.len() < 6 {
        return Ok(());
    }
    if seen.contains(&normalized) || memory_index.contains(&normalized) {
        return Ok(());
    }

    append_global(paths, &candidate, &tags)?;
    seen.insert(normalized.clone());
    memory_index.push_str(&normalized);
    added.push(candidate);
    Ok(())
}

fn extract_memory_candidates(input: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    for sentence in split_sentences(input) {
        if is_important_sentence(&sentence) {
            candidates.push(sentence);
        }
    }
    candidates
}

fn split_sentences(input: &str) -> Vec<String> {
    input
        .split(['\n', '。', '！', '？', '.', '!', '?', ';', '；'])
        .map(|chunk| {
            chunk
                .trim()
                .trim_matches(|c| c == '-' || c == '*' || c == '"' || c == '\'')
                .to_string()
        })
        .filter(|sentence| !sentence.is_empty())
        .collect()
}

fn is_important_sentence(sentence: &str) -> bool {
    let lowered = sentence.to_lowercase();
    let keywords = [
        "我希望",
        "我不希望",
        "我更喜欢",
        "偏好",
        "习惯",
        "请记住",
        "记住",
        "不要",
        "不希望",
        "必须",
        "一定要",
        "长期",
        "目标",
        "之后都",
        "以后都",
        "约束",
        "preference",
        "remember",
        "must",
        "always",
        "never",
    ];

    keywords.iter().any(|keyword| lowered.contains(keyword))
}

fn is_repeat_candidate(sentence: &str) -> bool {
    let count = sentence.chars().count();
    if !(8..=120).contains(&count) {
        return false;
    }
    let lowered = sentence.trim().to_lowercase();
    let trivial = ["你好", "谢谢", "好的", "嗯", "ok", "okay", "hi", "hello"];
    if trivial.iter().any(|word| lowered == *word) {
        return false;
    }
    !sentence.chars().all(|ch| ch.is_ascii_digit())
}

fn is_explicit_remember_sentence(sentence: &str) -> bool {
    let lowered = sentence.to_lowercase();
    lowered.contains("记住")
        || lowered.contains("请记")
        || lowered.contains("remember this")
        || lowered.contains("remember:")
        || lowered.starts_with("remember ")
}

fn count_short_term_occurrences(paths: &AgentPaths, sentence: &str) -> Result<usize> {
    let needle = normalize_for_compare(sentence);
    if needle.len() < 6 {
        return Ok(0);
    }

    let mut files = list_short_term_files(&paths.memory_dir)?;
    files.sort();
    files.reverse();

    let mut total = 0usize;
    for file in files.into_iter().take(30) {
        let content = fs::read_to_string(file).unwrap_or_default();
        let haystack = normalize_for_compare(&content);
        total += count_substring_occurrences(&haystack, &needle);
        if total >= 3 {
            break;
        }
    }
    Ok(total)
}

fn count_substring_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }

    let mut count = 0usize;
    let mut start = 0usize;
    while let Some(pos) = haystack[start..].find(needle) {
        count += 1;
        start += pos + needle.len();
    }
    count
}

fn infer_memory_tag(sentence: &str) -> &'static str {
    if sentence.contains("偏好")
        || sentence.contains("喜欢")
        || sentence.contains("我希望")
        || sentence.to_lowercase().contains("preference")
    {
        return "preference";
    }

    if sentence.contains("不要")
        || sentence.contains("不希望")
        || sentence.contains("必须")
        || sentence.contains("约束")
        || sentence.to_lowercase().contains("must")
    {
        return "constraint";
    }

    if sentence.contains("目标") || sentence.contains("长期") {
        return "goal";
    }

    "fact"
}

fn list_short_term_files(dir: &PathBuf) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file()
            && path.extension().map(|s| s == "md").unwrap_or(false)
            && is_daily_memory_file(&path)
        {
            files.push(path);
        }
    }
    Ok(files)
}

fn is_daily_memory_file(path: &PathBuf) -> bool {
    let Some(stem) = path.file_stem() else {
        return false;
    };
    let name = stem.to_string_lossy();
    let chars = name.chars().collect::<Vec<_>>();
    chars.len() == 10
        && chars[0].is_ascii_digit()
        && chars[1].is_ascii_digit()
        && chars[2].is_ascii_digit()
        && chars[3].is_ascii_digit()
        && chars[4] == '-'
        && chars[5].is_ascii_digit()
        && chars[6].is_ascii_digit()
        && chars[7] == '-'
        && chars[8].is_ascii_digit()
        && chars[9].is_ascii_digit()
}

fn normalize_for_compare(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|ch| {
            !ch.is_whitespace()
                && !matches!(
                    ch,
                    ',' | '，'
                        | '.'
                        | '。'
                        | ':'
                        | '：'
                        | ';'
                        | '；'
                        | '?'
                        | '？'
                        | '!'
                        | '！'
                        | '"'
                        | '\''
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '-'
                        | '_'
                )
        })
        .collect()
}

fn take_tail_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    input
        .chars()
        .rev()
        .take(max_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>()
}

fn strip_assistant_sections(input: &str) -> String {
    let mut out = String::new();
    let mut skipping_assistant = false;

    for line in input.lines() {
        let trimmed = line.trim();

        if trimmed.eq_ignore_ascii_case("assistant:") {
            skipping_assistant = true;
            continue;
        }

        if trimmed.starts_with("## ")
            || trimmed.starts_with("source:")
            || trimmed.eq_ignore_ascii_case("content:")
            || trimmed.eq_ignore_ascii_case("user:")
        {
            skipping_assistant = false;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if skipping_assistant {
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use uuid::Uuid;

    fn make_paths() -> AgentPaths {
        let root = std::env::temp_dir().join(format!("goldagent-memory-test-{}", Uuid::new_v4()));
        let memory_dir = root.join("memory");
        let logs_dir = root.join("logs");
        let skills_dir = root.join("skills");
        fs::create_dir_all(&memory_dir).unwrap();
        fs::create_dir_all(&logs_dir).unwrap();
        fs::create_dir_all(&skills_dir).unwrap();
        let memory_file = root.join("MEMORY.md");
        let jobs_file = root.join("jobs.json");
        let connect_file = root.join("connect.json");
        let usage_file = root.join("usage.json");
        fs::write(
            &memory_file,
            "# GoldAgent 长期记忆\n\n此文件用于保存长期、可复用的记忆。\n\n",
        )
        .unwrap();
        fs::write(&jobs_file, "[]\n").unwrap();
        fs::write(
            &connect_file,
            "{\n  \"provider\": \"openai\",\n  \"mode\": \"codex_login\",\n  \"model\": null,\n  \"api_key\": null\n}\n",
        )
        .unwrap();
        fs::write(
            &usage_file,
            "{\n  \"total\": {\"requests\": 0, \"input_tokens\": 0, \"output_tokens\": 0},\n  \"by_day\": {},\n  \"by_model\": {},\n  \"updated_at\": null\n}\n",
        )
        .unwrap();

        AgentPaths {
            root,
            memory_file,
            memory_dir,
            jobs_file,
            connect_file,
            usage_file,
            logs_dir,
            skills_dir,
        }
    }

    #[test]
    fn writes_short_term_daily_file() {
        let paths = make_paths();
        append_short_term(&paths, "test", "hello").unwrap();

        let expected = paths
            .memory_dir
            .join(format!("{}.md", Local::now().format("%Y-%m-%d")));
        assert!(expected.exists());

        let _ = fs::remove_dir_all(paths.root);
    }

    #[test]
    fn captures_event_to_long_term() {
        let paths = make_paths();
        let ok = auto_capture_event(&paths, "skill.new", "用户创建了技能：name=test").unwrap();
        assert!(ok);

        let memory = fs::read_to_string(&paths.memory_file).unwrap();
        assert!(memory.contains("用户创建了技能：name=test"));
        assert!(memory.contains("event"));

        let _ = fs::remove_dir_all(paths.root);
    }

    #[test]
    fn promotes_repeated_sentence_to_long_term() {
        let paths = make_paths();
        let sentence = "项目里日志统一写中文";
        for _ in 0..3 {
            append_short_term(
                &paths,
                "chat.turn",
                &format!("user:\n{sentence}\n\nassistant:\nok"),
            )
            .unwrap();
        }

        let added = auto_capture_long_term(&paths, "chat.turn", sentence).unwrap();
        assert!(!added.is_empty());

        let memory = fs::read_to_string(&paths.memory_file).unwrap();
        assert!(memory.contains(sentence));
        assert!(memory.contains("repeated"));

        let _ = fs::remove_dir_all(paths.root);
    }
}
