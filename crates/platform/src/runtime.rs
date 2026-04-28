use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

pub type CancelFlag = Arc<AtomicBool>;

static REGISTRY: std::sync::OnceLock<Mutex<HashMap<String, CancelFlag>>> =
    std::sync::OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, CancelFlag>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register(request_id: String) -> CancelFlag {
    let flag = Arc::new(AtomicBool::new(false));
    registry().lock().unwrap().insert(request_id, flag.clone());
    flag
}

pub fn cancel(request_id: &str) -> bool {
    if let Some(flag) = registry().lock().unwrap().get(request_id) {
        flag.store(true, Ordering::SeqCst);
        return true;
    }
    false
}

pub fn unregister(request_id: &str) {
    registry().lock().unwrap().remove(request_id);
}

pub fn is_cancelled(flag: &CancelFlag) -> bool {
    flag.load(Ordering::SeqCst)
}
