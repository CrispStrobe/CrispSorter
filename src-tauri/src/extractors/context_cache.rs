//! One bounded model context, replaced when its complete configuration changes.
use std::sync::{Mutex, MutexGuard, OnceLock};

pub struct CachedContext<T> {
    pub key: String,
    pub context: T,
}

pub fn acquire<T: 'static>(
    slot: &'static OnceLock<Mutex<Option<CachedContext<T>>>>,
    key: String,
    build: impl FnOnce() -> Result<T, String>,
) -> Result<MutexGuard<'static, Option<CachedContext<T>>>, String> {
    let mut guard = slot.get_or_init(|| Mutex::new(None)).lock().map_err(|e| format!("Model context lock poisoned: {e}"))?;
    if guard.as_ref().is_none_or(|cached| cached.key != key) {
        // Release the previous weights before allocating another model.
        drop(guard.take());
        let context = build()?;
        *guard = Some(CachedContext { key, context });
    }
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    struct Model { value: &'static str, drops: Arc<AtomicUsize> }
    impl Drop for Model { fn drop(&mut self) { self.drops.fetch_add(1, Ordering::SeqCst); } }
    #[test]
    fn changes_release_old_weights_and_failed_loads_can_be_retried() {
        let slot = Box::leak(Box::new(OnceLock::new()));
        let drops = Arc::new(AtomicUsize::new(0));
        {
            let cache = acquire(slot, "quick".into(), || Ok(Model { value: "quick", drops: drops.clone() })).unwrap();
            assert_eq!(cache.as_ref().unwrap().context.value, "quick");
        }
        drop(acquire(slot, "quick".into(), || Err("same key must reuse the loaded context".into())).unwrap());
        let failed = acquire(slot, "tables".into(), || {
            assert_eq!(drops.load(Ordering::SeqCst), 1);
            Err("missing model file".into())
        });
        assert!(failed.is_err());
        let recovered = acquire(slot, "tables".into(), || Ok(Model { value: "tables", drops })).unwrap();
        assert_eq!(recovered.as_ref().unwrap().context.value, "tables");
    }
}
