//! WakeupScheduler（架构 §4.12.2 / §4.12.4）：进程内的 cron + bg-task 唤醒中心。
//!
//! 独占一个后台线程 + current_thread runtime，跑三个 task + 一个 mpsc 通道：
//! - `CronTimer`：每秒扫 cron 表，到点投递 [`WakeupEvent::CronFired`]
//! - `BgFinishHook`：每秒扫 [`BackgroundShells`] 列表，发现已注册任务进入终态时投递
//!   [`WakeupEvent::BgTaskFinished`]
//! - `WakeupDispatcher`：消费 mpsc 事件，调用 surface 注册的 `ResumeHandler` 真正
//!   resume Run。Run 在挂起期间 agent_loop task 已经退出——dispatcher 帮它"再生"
//!
//! 独立 runtime 让调度器既不依赖 surface 的 runtime 上下文（避免 Tauri 同步 setup
//! 阶段调 `global()` 时 `tokio::spawn` 因无 reactor 而 panic），也不会随某个 surface
//! runtime 一起死。进程退出 = 整个 scheduler 一起死（§13 决策：不自动 resume 跨进程
//! 的 checkpoint）。

use std::sync::{Arc, Mutex, OnceLock};

use chrono::Utc;
use tokio::sync::mpsc;

use crate::storage::run_checkpoint::RunPhase;
use crate::tools::background::BackgroundShells;

/// PhaseChannel：dispatcher 与 agent_loop 之间共享的"当前 ToolStep 跑完后要不要挂起"
/// 标志位。WaitForTask / ScheduleWakeup 工具执行时写入；agent_loop 在 ToolStep
/// 完成后取出并处理（架构 §4.12.4）。
pub type PhaseChannel = Arc<Mutex<Option<RunPhase>>>;

pub fn new_phase_channel() -> PhaseChannel {
    Arc::new(Mutex::new(None))
}

/// 调度器内部事件，由后台 task 投递、由 [`WakeupDispatcher`] 消费。
#[derive(Debug, Clone)]
pub enum WakeupEvent {
    BgTaskFinished {
        session_id: String,
        run_id: String,
        task_id: String,
        exit_code: Option<i32>,
        duration_ms: u64,
    },
    CronFired {
        session_id: String,
        run_id: String,
        scheduled_for_ms: i64,
        reason: String,
    },
}

impl WakeupEvent {
    pub fn session_id(&self) -> &str {
        match self {
            WakeupEvent::BgTaskFinished { session_id, .. }
            | WakeupEvent::CronFired { session_id, .. } => session_id,
        }
    }

    pub fn run_id(&self) -> &str {
        match self {
            WakeupEvent::BgTaskFinished { run_id, .. } | WakeupEvent::CronFired { run_id, .. } => {
                run_id
            }
        }
    }
}

/// 注册到 scheduler 的 resume 回调。App 层（desktop / cli）实现它——拿
/// `session_id` 去查 session 配置、构造 client、读 checkpoint、rebuild transcript、
/// 注入 `<wakeup>` user message、调 `Harness::spawn_run`。
pub type ResumeHandler = Arc<dyn Fn(WakeupEvent) + Send + Sync + 'static>;

struct Cron {
    fire_at_ms: i64,
    session_id: String,
    run_id: String,
    reason: String,
}

#[derive(Clone)]
struct BgWatch {
    task_id: String,
    session_id: String,
    run_id: String,
}

#[derive(Default)]
struct SchedulerInner {
    crons: Vec<Cron>,
    bg_watches: Vec<BgWatch>,
    handler: Option<ResumeHandler>,
    /// session-scoped BackgroundShells 引用（架构 §4.12.2 修订）。BgFinishHook
    /// 用 BgWatch.session_id 反查，找不到说明该 session 已被销毁——直接当 done。
    session_shells: std::collections::HashMap<String, BackgroundShells>,
}

pub struct WakeupScheduler {
    inner: Mutex<SchedulerInner>,
    tx: mpsc::UnboundedSender<WakeupEvent>,
}

static GLOBAL: OnceLock<Arc<WakeupScheduler>> = OnceLock::new();

impl WakeupScheduler {
    /// 进程级单例。首次调用启动三个后台 task。
    pub fn global() -> Arc<Self> {
        GLOBAL
            .get_or_init(|| {
                let (tx, rx) = mpsc::unbounded_channel();
                let s = Arc::new(WakeupScheduler {
                    inner: Mutex::new(SchedulerInner::default()),
                    tx,
                });
                s.clone().start_background_tasks(rx);
                s
            })
            .clone()
    }

    fn start_background_tasks(self: Arc<Self>, rx: mpsc::UnboundedReceiver<WakeupEvent>) {
        // 调度器独占一个 current_thread runtime，和 surface（Tauri / CLI）的 runtime
        // 完全隔离。否则若调用方在无 runtime 上下文里首次触发 `global()`（典型例子：
        // Tauri 的同步 setup 闭包跑在 NSApplication did_finish_launching 主线程上），
        // 内部 `tokio::spawn` 会立刻 panic；ObjC 回调禁止 unwind，会把 panic 升级为
        // 进程 abort。独立 runtime 同时让 scheduler 的生命周期挂在进程而非任一 surface。
        let s_cron = self.clone();
        let s_bg = self.clone();
        let s_disp = self.clone();
        std::thread::Builder::new()
            .name("wakeup-scheduler".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build wakeup scheduler runtime");
                rt.block_on(async move {
                    let mut rx = rx;
                    tokio::spawn(async move {
                        let mut tick =
                            tokio::time::interval(std::time::Duration::from_millis(1000));
                        loop {
                            tick.tick().await;
                            s_cron.scan_cron();
                        }
                    });
                    tokio::spawn(async move {
                        let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
                        loop {
                            tick.tick().await;
                            s_bg.scan_bg();
                        }
                    });
                    while let Some(ev) = rx.recv().await {
                        let handler = s_disp.inner.lock().unwrap().handler.clone();
                        if let Some(h) = handler {
                            h(ev);
                        } else {
                            tracing::warn!("wakeup event arrived but no resume_handler registered");
                        }
                    }
                });
            })
            .expect("spawn wakeup scheduler thread");
    }

    fn scan_cron(&self) {
        let now = Utc::now().timestamp_millis();
        let fired: Vec<Cron> = {
            let mut inner = self.inner.lock().unwrap();
            let (fire, keep): (Vec<_>, Vec<_>) =
                inner.crons.drain(..).partition(|c| c.fire_at_ms <= now);
            inner.crons = keep;
            fire
        };
        for c in fired {
            let _ = self.tx.send(WakeupEvent::CronFired {
                session_id: c.session_id,
                run_id: c.run_id,
                scheduled_for_ms: c.fire_at_ms,
                reason: c.reason,
            });
        }
    }

    fn scan_bg(&self) {
        let (watches, session_shells) = {
            let inner = self.inner.lock().unwrap();
            (inner.bg_watches.clone(), inner.session_shells.clone())
        };
        let mut still: Vec<BgWatch> = Vec::new();
        for w in watches {
            // 按 session 路由：找到该 session 的 BackgroundShells，再按 task_id 查。
            // 找不到 shells（session 已销毁）或找不到 task（被 GC）→ 当 done 兜底。
            let shells_for_session = session_shells.get(&w.session_id);
            let (done, exit_code, duration_ms) = match shells_for_session {
                Some(shells) => match shells.get(&w.task_id) {
                    Some(s) => {
                        let terminal = s.state().is_terminal();
                        let code = match s.state() {
                            crate::tools::background::ShellState::Exited { code } => code,
                            _ => None,
                        };
                        let dur = s.started_at.elapsed().as_millis() as u64;
                        (terminal, code, dur)
                    }
                    None => (true, None, 0),
                },
                None => (true, None, 0),
            };
            if done {
                let _ = self.tx.send(WakeupEvent::BgTaskFinished {
                    session_id: w.session_id,
                    run_id: w.run_id,
                    task_id: w.task_id,
                    exit_code,
                    duration_ms,
                });
            } else {
                still.push(w);
            }
        }
        self.inner.lock().unwrap().bg_watches = still;
    }

    /// 在 cron 表里登记一条；到 `fire_at_ms` 时投递事件。
    pub fn arm_cron(&self, session_id: String, run_id: String, fire_at_ms: i64, reason: String) {
        self.inner.lock().unwrap().crons.push(Cron {
            fire_at_ms,
            session_id,
            run_id,
            reason,
        });
    }

    /// 在 bg_watches 表里登记一条；BgFinishHook 发现 task 进入终态时投递事件。
    pub fn arm_bg_task(&self, session_id: String, run_id: String, task_id: String) {
        self.inner.lock().unwrap().bg_watches.push(BgWatch {
            task_id,
            session_id,
            run_id,
        });
    }

    /// 注册某个 session 的 BackgroundShells，BgFinishHook 用它扫该 session 的
    /// 后台任务终态。同一 session_id 多次注册以最后一次为准——chat() 每次调用都
    /// 重新登记没问题（同 session 拿到的是同一个 Arc 视图）。
    pub fn register_session_shells(&self, session_id: String, shells: BackgroundShells) {
        self.inner
            .lock()
            .unwrap()
            .session_shells
            .insert(session_id, shells);
    }

    /// session 关闭时摘除登记，让 BgFinishHook 不再扫这个 session。
    pub fn unregister_session_shells(&self, session_id: &str) {
        self.inner.lock().unwrap().session_shells.remove(session_id);
    }

    /// App 层注册 resume 回调。回调拿 [`WakeupEvent`] 自己去 spawn_run。
    pub fn set_resume_handler(&self, handler: ResumeHandler) {
        self.inner.lock().unwrap().handler = Some(handler);
    }

    /// session 被 cancel / Finished 时调用，清理未消费的 watch / cron。
    pub fn discard_run(&self, session_id: &str, run_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner
            .crons
            .retain(|c| !(c.session_id == session_id && c.run_id == run_id));
        inner
            .bg_watches
            .retain(|w| !(w.session_id == session_id && w.run_id == run_id));
    }

    /// 列出指定 session 当前还在等的 cron（distant：fire_at_ms - now）。
    /// surface 用它在 BackgroundTaskPanel 渲染「定时唤醒倒计时」。
    pub fn list_pending_crons(&self, session_id: &str) -> Vec<PendingCron> {
        let now = Utc::now().timestamp_millis();
        self.inner
            .lock()
            .unwrap()
            .crons
            .iter()
            .filter(|c| c.session_id == session_id)
            .map(|c| PendingCron {
                run_id: c.run_id.clone(),
                fire_at_ms: c.fire_at_ms,
                seconds_remaining: ((c.fire_at_ms - now).max(0) / 1000) as u64,
                reason: c.reason.clone(),
            })
            .collect()
    }
}

/// `WakeupScheduler::list_pending_crons` 的返回项。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingCron {
    pub run_id: String,
    pub fire_at_ms: i64,
    pub seconds_remaining: u64,
    pub reason: String,
}

/// 给 `<wakeup>` user message 用的 XML 拼装（架构 §4.12.5）。
pub fn wakeup_xml(event: &WakeupEvent) -> String {
    match event {
        WakeupEvent::BgTaskFinished {
            task_id,
            exit_code,
            duration_ms,
            ..
        } => format!(
            "<wakeup kind=\"bg_task_finished\" task_id=\"{task_id}\" exit_code=\"{}\" duration_ms=\"{duration_ms}\">\n后台任务已完成。\n</wakeup>",
            exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
        ),
        WakeupEvent::CronFired {
            scheduled_for_ms,
            reason,
            ..
        } => {
            let iso = chrono::DateTime::<Utc>::from_timestamp_millis(*scheduled_for_ms)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default();
            format!(
                "<wakeup kind=\"cron_fired\" scheduled_for=\"{iso}\" original_reason=\"{reason}\">\n定时已到，按你之前 ScheduleWakeup 的设定唤醒。\n</wakeup>",
            )
        }
    }
}
