use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AgentPaths {
    pub root: PathBuf,
    pub memory_file: PathBuf,
    pub memory_dir: PathBuf,
    pub jobs_file: PathBuf,
    pub connect_file: PathBuf,
    pub usage_file: PathBuf,
    pub logs_dir: PathBuf,
    pub skills_dir: PathBuf,
}

impl AgentPaths {
    pub fn new() -> Result<Self> {
        let root = if let Ok(path) = env::var("GOLDAGENT_HOME") {
            PathBuf::from(path)
        } else {
            let home = dirs::home_dir().context("无法解析 HOME 目录")?;
            home.join(".goldagent")
        };

        Ok(Self {
            memory_file: root.join("MEMORY.md"),
            memory_dir: root.join("memory"),
            jobs_file: root.join("jobs.json"),
            connect_file: root.join("connect.json"),
            usage_file: root.join("usage.json"),
            logs_dir: root.join("logs"),
            skills_dir: root.join("skills"),
            root,
        })
    }

    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        fs::create_dir_all(&self.memory_dir)?;
        fs::create_dir_all(&self.logs_dir)?;
        fs::create_dir_all(&self.skills_dir)?;

        ensure_file_with(
            &self.memory_file,
            "# GoldAgent 长期记忆\n\n此文件用于保存长期、可复用的记忆。\n\n",
        )?;
        ensure_file_with(&self.jobs_file, "[]\n")?;
        ensure_file_with(
            &self.connect_file,
            "{\n  \"provider\": \"openai\",\n  \"mode\": \"codex_login\",\n  \"model\": null,\n  \"api_key\": null,\n  \"zhipu_api_type\": \"coding\"\n}\n",
        )?;
        ensure_file_with(
            &self.usage_file,
            "{\n  \"total\": {\"requests\": 0, \"input_tokens\": 0, \"output_tokens\": 0},\n  \"by_day\": {},\n  \"by_model\": {},\n  \"updated_at\": null\n}\n",
        )?;
        self.seed_default_skill()?;
        Ok(())
    }

    fn seed_default_skill(&self) -> Result<()> {
        let skill_dir = self.skills_dir.join("daily-summary");
        fs::create_dir_all(&skill_dir)?;
        let skill_file = skill_dir.join("SKILL.md");
        ensure_file_with(
            &skill_file,
            "# daily-summary\n\n元信息：\n- 名称：daily-summary\n- 版本：v1\n- 描述：将用户当天的信息整理为简洁总结与下一步行动。\n- 适用场景：用户要求复盘、日结、行动项整理。\n\n输入：\n- 用户输入：当天发生的事项、会议、任务、感受等。\n- 上下文：近期记忆与历史待办。\n\n输出：\n- 产出格式：先给总结，再给 3 条下一步行动。\n- 质量要求：简洁、清晰、可执行。\n\n执行步骤：\n1. 阅读输入并提取关键事件。\n2. 生成要点式总结。\n3. 给出 3 条最优先的下一步行动。\n\n约束：\n- 保持简洁。\n- 优先使用可执行的行动语言。\n- 不编造未提及事实。\n\n失败处理：\n- 信息不足时，明确缺失点并给出最小可执行建议。\n\n示例：\n输入：今天完成了需求评审和接口联调。\n输出：\n1. 总结：...\n2. 下一步行动：...\n",
        )
    }
}

fn ensure_file_with(path: &PathBuf, default_content: &str) -> Result<()> {
    if !path.exists() {
        fs::write(path, default_content)?;
    }
    Ok(())
}
