use crate::config::AgentPaths;
use crate::memory;
use crate::openai::{ChatMessage, OpenAIClient};
use anyhow::{Result, bail};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

pub fn list_skills(paths: &AgentPaths) -> Result<Vec<SkillInfo>> {
    if !paths.skills_dir.exists() {
        return Ok(Vec::new());
    }

    let mut skills = Vec::new();
    for entry in fs::read_dir(&paths.skills_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        let skill_md = path.join("SKILL.md");
        if !skill_md.exists() {
            continue;
        }

        let content = fs::read_to_string(&skill_md).unwrap_or_default();
        let description = extract_description(&content);

        skills.push(SkillInfo {
            name,
            description,
            path: skill_md,
        });
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(skills)
}

pub fn create_skill(paths: &AgentPaths, name: &str) -> Result<PathBuf> {
    let skill_name = normalize_skill_name(name);
    if skill_name.is_empty() {
        bail!("技能名称不能为空");
    }

    let skill_dir = paths.skills_dir.join(&skill_name);
    if skill_dir.exists() {
        bail!("技能 `{}` 已存在", skill_name);
    }
    fs::create_dir_all(&skill_dir)?;

    let skill_file = skill_dir.join("SKILL.md");
    let template = format!(
        "# {skill_name}\n\n\
元信息：\n\
- 名称：{skill_name}\n\
- 版本：v1\n\
- 描述：请在此处填写这个技能的目标与价值。\n\
- 适用场景：请在此处填写什么时候触发这个技能。\n\n\
输入：\n\
- 用户输入：自然语言或结构化参数。\n\
- 上下文：可选的记忆、系统状态或外部事件。\n\n\
输出：\n\
- 产出格式：请明确输出结构（例如：要点列表、JSON、步骤计划）。\n\
- 质量要求：准确、简洁、可执行。\n\n\
执行步骤：\n\
1. 解析输入并识别任务目标。\n\
2. 补全必要上下文，缺失信息时先做合理假设并标注。\n\
3. 生成结果并进行一次自检（是否满足输出格式与约束）。\n\n\
约束：\n\
- 禁止输出无法验证的事实。\n\
- 优先给出可执行建议。\n\
- 涉及高风险操作时，先提示风险与确认步骤。\n\n\
失败处理：\n\
- 当信息不足：明确说明缺失项并给出最小可执行方案。\n\
- 当执行失败：输出错误原因、影响范围和下一步恢复建议。\n\n\
示例：\n\
输入：请总结今天会议并给出三条行动项。\n\
输出：\n\
1. 会议总结：...\n\
2. 行动项：...\n\
3. 风险与跟进：...\n"
    );
    fs::write(&skill_file, template)?;
    Ok(skill_file)
}

pub async fn run_skill(
    paths: &AgentPaths,
    client: &OpenAIClient,
    name: &str,
    input: &str,
) -> Result<String> {
    let skill_file = paths.skills_dir.join(name).join("SKILL.md");
    if !skill_file.exists() {
        bail!("Skill `{name}` not found in {}", paths.skills_dir.display());
    }

    let skill_content = fs::read_to_string(&skill_file)?;
    let memory_context = memory::tail_context(paths, 3_000)?;

    let system = format!(
        "You are GoldAgent.\n\
Current backend: {}.\n\
If asked about model/backend identity, answer strictly based on Current backend, not historical memory.\n\n\
Skill definition:\n{skill_content}\n\nMemory context:\n{memory_context}\n\n\
Follow the skill faithfully and produce a concise response.",
        client.backend_label()
    );

    let messages = vec![ChatMessage::system(system), ChatMessage::user(input)];
    let response = client.chat(&messages).await?;
    Ok(response)
}

fn extract_description(content: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("- 描述：") {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
        if let Some(value) = trimmed.strip_prefix("描述：") {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }

    content
        .lines()
        .skip_while(|line| line.trim().is_empty() || line.trim().starts_with('#'))
        .find(|line| !line.trim().is_empty())
        .unwrap_or("无描述")
        .to_string()
}

fn normalize_skill_name(name: &str) -> String {
    name.trim()
        .replace(' ', "-")
        .replace('/', "-")
        .replace('\\', "-")
}
