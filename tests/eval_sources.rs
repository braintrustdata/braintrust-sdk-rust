use braintrust_sdk_rust::eval::{
    Case, CaseSource, IterCaseSource, StreamCaseSource, VecCaseSource,
};
use std::{
    cell::Cell,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

fn case<I, O>(input: I) -> Case<I, O> {
    Case {
        input,
        expected: None,
        metadata: None,
        tags: None,
        id: None,
        xact_id: None,
        created: None,
    }
}
struct OwnedInput {
    value: Cell<usize>,
    drops: Arc<AtomicUsize>,
}
impl Drop for OwnedInput {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn vectors_move_nonclone_nonsync_cases_and_release_consumed_values() {
    futures::executor::block_on(async {
        let drops = Arc::new(AtomicUsize::new(0));
        let cases: Vec<Case<OwnedInput, Cell<usize>>> = (0..2)
            .map(|value| {
                case(OwnedInput {
                    value: Cell::new(value),
                    drops: drops.clone(),
                })
            })
            .collect();
        let mut source: Box<dyn CaseSource<OwnedInput, Cell<usize>>> = cases.into();
        let first = source.next().await.unwrap().unwrap();
        assert_eq!(first.input.value.get(), 0);
        drop(first);
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "the source must release consumed cases"
        );
        drop(source);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    });
}

#[derive(Debug, thiserror::Error)]
#[error("read failed")]
struct ReadFailure;

#[tokio::test]
async fn fallible_iterators_keep_error_causes_and_do_not_require_sync() {
    type LocalCase = Case<Cell<usize>, Cell<usize>>;
    let items: Vec<anyhow::Result<LocalCase>> =
        vec![Ok(case(Cell::new(7))), Err(ReadFailure.into())];
    let mut source: Box<dyn CaseSource<Cell<usize>, Cell<usize>>> =
        Box::new(IterCaseSource::new(items.into_iter()));
    assert_eq!(source.next().await.unwrap().unwrap().input.get(), 7);
    assert!(source
        .next()
        .await
        .unwrap_err()
        .downcast_ref::<ReadFailure>()
        .is_some());
    assert!(source.next().await.unwrap().is_none());
}

#[tokio::test]
async fn pinned_streams_allow_nonunpin_nonsync_state_and_owned_error_causes() {
    let value = Cell::new(8);
    let stream = futures::stream::once(async move {
        value.set(value.get() + 1);
        tokio::task::yield_now().await;
        Ok::<Case<Cell<usize>, Cell<usize>>, anyhow::Error>(case(value))
    });
    let mut source: Box<dyn CaseSource<Cell<usize>, Cell<usize>>> =
        Box::new(StreamCaseSource::new(stream));
    assert_eq!(source.next().await.unwrap().unwrap().input.get(), 9);
    assert!(source.next().await.unwrap().is_none());
    let stream =
        futures::stream::once(async { Err::<Case<(), ()>, anyhow::Error>(ReadFailure.into()) });
    let mut source = StreamCaseSource::new(stream);
    assert!(source
        .next()
        .await
        .unwrap_err()
        .downcast_ref::<ReadFailure>()
        .is_some());
}

#[tokio::test]
async fn empty_and_infallible_sources_end_without_synthetic_cases() {
    let mut empty = VecCaseSource::<usize, usize>::new(vec![]);
    assert!(empty.next().await.unwrap().is_none());
    assert!(empty.next().await.unwrap().is_none());
    let mut source =
        IterCaseSource::new((0..2).map(|value| Ok::<_, anyhow::Error>(case::<_, usize>(value))));
    assert_eq!(source.next().await.unwrap().unwrap().input, 0);
    assert_eq!(source.next().await.unwrap().unwrap().input, 1);
    assert!(source.next().await.unwrap().is_none());
}
