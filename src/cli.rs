use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "goldagent", version, about = "GoldAgent 本地命令行助手")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// 初始化 GoldAgent 数据目录
    Init,
    /// 启动循环对话会话
    Chat {
        #[arg(long)]
        model: Option<String>,
    },
    /// 让模型执行一次单轮任务
    Run {
        task: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// 触发一次本地提醒（可用于定时任务）
    Remind { message: String },
    /// 启动后台定时任务服务
    Serve,
    /// 执行一条 shell 命令
    Shell {
        cmd: String,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// 连接模型后端（登录态/API Key）
    Connect {
        #[command(subcommand)]
        command: ConnectCommand,
    },
    /// Cron 定时任务命令
    Cron {
        #[command(subcommand)]
        command: CronCommand,
    },
    /// Hook 事件触发任务命令
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
    /// Skill 技能命令
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum CronCommand {
    /// 新增一条 cron 任务
    Add {
        schedule: String,
        command: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 1)]
        retry_max: u8,
    },
    /// 列出所有 cron 任务
    List,
    /// 删除一条 cron 任务
    Remove { id: String },
}

#[derive(Debug, Subcommand)]
pub enum HookCommand {
    /// 新增 Git 提交轮询触发任务
    AddGit {
        repo: String,
        command: String,
        #[arg(long = "ref")]
        reference: Option<String>,
        #[arg(long, default_value_t = 30)]
        interval: u64,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 1)]
        retry_max: u8,
    },
    /// 新增 P4 提交轮询触发任务
    AddP4 {
        depot: String,
        command: String,
        #[arg(long, default_value_t = 30)]
        interval: u64,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 1)]
        retry_max: u8,
    },
    /// 列出所有 hook 任务
    List,
    /// 删除一条 hook 任务
    Remove { id: String },
}

#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// 列出已安装的技能
    List,
    /// 创建一个新的技能模板
    New { name: String },
    /// 运行一个技能并传入输入内容
    Run {
        name: String,
        input: String,
        #[arg(long)]
        model: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConnectCommand {
    /// 查看当前连接状态
    Status,
    /// 使用登录态（可选指定 model）
    Login {
        #[arg(long)]
        model: Option<String>,
    },
    /// 使用 API Key（可通过 --provider 选择厂商）
    Api {
        api_key: String,
        #[arg(long, default_value = "openai")]
        provider: String,
        #[arg(long)]
        zhipu_api_type: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
}
