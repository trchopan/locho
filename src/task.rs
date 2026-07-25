use tokio::task::JoinSet;
use tracing::error;

pub fn reap_finished<T: 'static>(tasks: &mut JoinSet<T>, task_kind: &'static str) -> usize {
    let mut reaped = 0;
    while let Some(result) = tasks.try_join_next() {
        reaped += 1;
        if let Err(error) = result {
            error!(%error, task_kind, "task failed");
        }
    }
    reaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reaps_completed_tasks_and_panics() {
        let mut tasks = JoinSet::new();
        tasks.spawn(async {});
        tasks.spawn(async { panic!("test task panic") });

        for _ in 0..10 {
            tokio::task::yield_now().await;
            if tasks.is_empty() {
                break;
            }
        }

        assert_eq!(reap_finished(&mut tasks, "test"), 2);
        assert!(tasks.is_empty());
    }
}
