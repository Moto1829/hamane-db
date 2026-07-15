//! セグメント並列検索用の常駐スレッドプール (todo 801)。
//!
//! 503 の検索ごとの `std::thread::scope` を置き換える。Database 全体で
//! 1 個を共有し、スレッド生成コストの排除と並列度の上限を両立する。
//! worker は初回の `execute` まで起動しない (検索しない用途で
//! スレッドを持たないため)。

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

pub(crate) type Job = Box<dyn FnOnce() + Send + 'static>;

/// 固定サイズのワーカープール。並列度は「呼び出しスレッド + worker 数」。
pub(crate) struct SearchPool {
    /// 実効並列度 (呼び出しスレッドを含む)。worker 数はこれより 1 少ない
    threads: usize,
    inner: OnceLock<Inner>,
}

struct Inner {
    sender: Sender<Job>,
    workers: Vec<JoinHandle<()>>,
}

impl SearchPool {
    /// `search_threads` (0 = 自動 = 論理コア数) から作る。worker は未起動。
    pub(crate) fn new(search_threads: usize) -> Self {
        let threads = if search_threads == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        } else {
            search_threads
        };
        Self {
            threads,
            inner: OnceLock::new(),
        }
    }

    /// 実効並列度。1 の場合、呼び出し元は逐次実行すること (worker が
    /// 存在せずジョブが実行されない)。
    pub(crate) fn threads(&self) -> usize {
        self.threads
    }

    /// ジョブをキューへ積む。空き worker がなければ実行は先着順に待つ。
    ///
    /// ジョブ内の panic は worker を殺さない。呼び出し元へ伝播させたい場合は
    /// ジョブ側で catch_unwind して結果チャネルに載せること。
    pub(crate) fn execute(&self, job: Job) {
        debug_assert!(self.threads > 1, "threads == 1 では worker が存在しない");
        let inner = self.inner.get_or_init(|| Inner::start(self.threads - 1));
        inner
            .sender
            .send(job)
            .expect("search pool workers never exit while pool is alive");
    }
}

impl Inner {
    fn start(worker_count: usize) -> Self {
        let (sender, receiver) = channel::<Job>();
        let receiver = Arc::new(Mutex::new(receiver));
        let workers = (0..worker_count)
            .map(|i| {
                let receiver = Arc::clone(&receiver);
                std::thread::Builder::new()
                    .name(format!("hamane-search-{i}"))
                    .spawn(move || loop {
                        // recv を待つ間だけロックを持つ (実行中は他 worker が取れる)
                        let job = receiver.lock().expect("lock poisoned").recv();
                        match job {
                            Ok(job) => {
                                let _ = catch_unwind(AssertUnwindSafe(job));
                            }
                            // sender が落ちた = プール破棄。worker 終了
                            Err(_) => break,
                        }
                    })
                    .expect("failed to spawn search worker")
            })
            .collect();
        Self { sender, workers }
    }
}

impl Drop for SearchPool {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            drop(inner.sender);
            for worker in inner.workers {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    #[test]
    fn runs_jobs_on_workers() {
        let pool = SearchPool::new(4);
        let counter = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel();
        for _ in 0..100 {
            let (counter, tx) = (Arc::clone(&counter), tx.clone());
            pool.execute(Box::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
                tx.send(()).unwrap();
            }));
        }
        for _ in 0..100 {
            rx.recv().unwrap();
        }
        assert_eq!(counter.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn worker_survives_job_panic() {
        let pool = SearchPool::new(2); // worker 1 本。panic 後も同じ worker が動くこと
        let (tx, rx) = mpsc::channel();
        pool.execute(Box::new(|| panic!("boom")));
        let tx2 = tx.clone();
        pool.execute(Box::new(move || tx2.send(()).unwrap()));
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker died after panic");
    }

    #[test]
    fn auto_threads_is_at_least_one() {
        assert!(SearchPool::new(0).threads() >= 1);
    }

    #[test]
    fn drop_without_use_spawns_nothing() {
        // 遅延初期化: execute しなければ Drop は何も join しない (即座に返る)
        let pool = SearchPool::new(8);
        drop(pool);
    }
}
