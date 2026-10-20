//! One admitted, demand-driven dataset cursor on its resource's fixed binding.
use super::*;
use std::future::IntoFuture;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone)]
pub(crate) struct ReadError(Arc<BraintrustError>);
impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}
impl From<BraintrustError> for ReadError {
    fn from(error: BraintrustError) -> Self {
        Self(Arc::new(error))
    }
}
impl From<crate::ExecutionError> for ReadError {
    fn from(error: crate::ExecutionError) -> Self {
        BraintrustError::from(error).into()
    }
}
impl From<crate::EnqueueError> for ReadError {
    fn from(error: crate::EnqueueError) -> Self {
        BraintrustError::from(error).into()
    }
}

type Reply = oneshot::Sender<std::result::Result<Option<DatasetRecord>, ReadError>>;
type Driver = futures::future::Shared<BoxFuture<'static, std::result::Result<(), ReadError>>>;

pub(crate) struct ReadSession {
    requests: Option<mpsc::Sender<Reply>>,
    ready: Option<oneshot::Receiver<DatasetMetadata>>,
    metadata: Option<DatasetMetadata>,
    driver: Driver,
    done: bool,
}
impl ReadSession {
    pub fn start(
        dataset: Dataset,
        batch_size: Option<usize>,
        version: Option<String>,
    ) -> std::result::Result<Self, ReadError> {
        let (requests, mut receiver) = mpsc::channel::<Reply>(1);
        let (ready, metadata) = oneshot::channel();
        let execution = dataset.execution.clone();
        let completion = execution.response(async move {
            let metadata = dataset.ensure_registered().await?.clone();
            let mut cursor = DatasetCursor {
                fetcher: dataset.api.clone(),
                scope: dataset.scope.clone(),
                dataset_id: metadata.dataset_id.clone(),
                batch_size: batch_size.unwrap_or(1000).max(1),
                version,
                cursor: None,
                buffer: VecDeque::new(),
                done: false,
            };
            if ready.send(metadata).is_err() {
                return Ok(());
            }
            while let Some(reply) = receiver.recv().await {
                let result = cursor.next().await.map_err(ReadError::from);
                let terminal = !matches!(result, Ok(Some(_)));
                let failure = result.as_ref().err().cloned();
                if reply.send(result).is_err() {
                    break;
                }
                if let Some(error) = failure {
                    return Err(error);
                }
                if terminal {
                    break;
                }
            }
            Ok(())
        });
        if let Err(error) = &*completion {
            return Err(error.clone());
        }
        Ok(Self {
            requests: Some(requests),
            ready: Some(metadata),
            metadata: None,
            driver: completion.into_future().boxed().shared(),
            done: false,
        })
    }

    pub async fn prepare(&mut self) -> std::result::Result<&DatasetMetadata, ReadError> {
        if let Some(ready) = self.ready.take() {
            self.metadata = Some(self.receive(ready).await?);
        }
        self.metadata.as_ref().ok_or_else(stopped)
    }

    pub async fn next(&mut self) -> std::result::Result<Option<DatasetRecord>, ReadError> {
        self.prepare().await?;
        if self.done {
            return Ok(None);
        }
        let (reply, received) = oneshot::channel();
        if self
            .requests
            .as_ref()
            .ok_or_else(stopped)?
            .send(reply)
            .await
            .is_err()
        {
            return Err(self.driver.clone().await.err().unwrap_or_else(stopped));
        }
        let result = self.receive(received).await?;
        let result = result?;
        if result.is_none() {
            self.done = true;
            self.requests = None;
        }
        Ok(result)
    }

    async fn receive<T>(
        &self,
        receiver: oneshot::Receiver<T>,
    ) -> std::result::Result<T, ReadError> {
        use futures::future::{select, Either};
        // Prefer a reply already produced before a terminal driver result.
        match select(receiver, self.driver.clone()).await {
            Either::Left((Ok(value), _)) => Ok(value),
            Either::Left((Err(_), driver)) => Err(driver.await.err().unwrap_or_else(stopped)),
            Either::Right((result, _)) => Err(result.err().unwrap_or_else(stopped)),
        }
    }
}
fn stopped() -> ReadError {
    BraintrustError::Background("dataset read session stopped before replying".into()).into()
}
