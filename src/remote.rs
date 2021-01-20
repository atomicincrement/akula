use self::kv_client::*;
use crate::traits::{self, Cursor as _};
use anyhow::Context;
use async_stream::stream;
use async_trait::async_trait;
use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::{
    mpsc::{channel, Sender},
    oneshot::{channel as oneshot, Sender as OneshotSender},
    Mutex as AsyncMutex,
};
use tokio_stream::StreamExt;
use tonic::{body::BoxBody, client::GrpcService, codegen::HttpBody, Streaming};
use tracing::*;

tonic::include_proto!("remote");

/// Remote transaction type via gRPC interface.
pub struct RemoteTransaction {
    // Invariant: cannot send new message until we process response to it.
    io: Arc<AsyncMutex<(Sender<Cursor>, Streaming<Pair>)>>,
}

/// Cursor opened by `RemoteTransaction`.
pub struct RemoteCursor<'tx> {
    transaction: &'tx RemoteTransaction,
    id: u32,

    #[allow(unused)]
    drop_handle: OneshotSender<()>,
}

#[async_trait]
impl crate::Transaction for RemoteTransaction {
    type Cursor<'tx> = RemoteCursor<'tx>;
    type CursorDupSort<'tx> = RemoteCursor<'tx>;
    type CursorDupFixed<'tx> = RemoteCursor<'tx>;

    async fn cursor<'tx>(&'tx self, bucket_name: &'tx str) -> anyhow::Result<Self::Cursor<'tx>> {
        // - send op open
        // - get cursor id
        let mut s = self.io.lock().await;

        let bucket_name = bucket_name.to_string();

        trace!("Sending request to open cursor");

        s.0.send(Cursor {
            op: Op::Open as i32,
            bucket_name: bucket_name.clone(),
            cursor: Default::default(),
            k: Default::default(),
            v: Default::default(),
        })
        .await?;

        let id = s.1.message().await?.context("no response")?.cursor_id;

        trace!("Opened cursor {}", id);

        drop(s);

        let (drop_handle, drop_rx) = oneshot();

        tokio::spawn({
            let io = self.io.clone();
            async move {
                let _ = drop_rx.await;
                let mut io = io.lock().await;

                trace!("Closing cursor {}", id);
                let _ =
                    io.0.send(Cursor {
                        op: Op::Close as i32,
                        cursor: id,
                        bucket_name: Default::default(),
                        k: Default::default(),
                        v: Default::default(),
                    })
                    .await;
                let _ = io.1.next().await;
            }
        });

        Ok(RemoteCursor {
            transaction: self,
            drop_handle,
            id,
        })
    }

    async fn cursor_dup_sort<'tx>(
        &'tx self,
        bucket_name: &'tx str,
    ) -> anyhow::Result<Self::CursorDupSort<'tx>> {
        self.cursor(bucket_name).await
    }

    async fn cursor_dup_fixed<'tx>(
        &'tx self,
        bucket_name: &'tx str,
    ) -> anyhow::Result<Self::CursorDupFixed<'tx>> {
        self.cursor(bucket_name).await
    }

    async fn get_one(&self, bucket: &str, key: &[u8]) -> anyhow::Result<Bytes>
    where
        Self: Sync,
    {
        let mut cursor = self.cursor(bucket).await?;

        Ok(cursor.seek_exact(key).await?.1)
    }

    async fn has_one(&self, bucket: &str, key: &[u8]) -> anyhow::Result<bool> {
        let mut cursor = self.cursor(bucket).await?;

        Ok(key == cursor.seek(key).await?.0)
    }
}

impl<'tx> RemoteCursor<'tx> {
    async fn op(
        &mut self,
        op: Op,
        key: Option<&[u8]>,
        value: Option<&[u8]>,
    ) -> anyhow::Result<(Bytes, Bytes)> {
        let mut io = self.transaction.io.lock().await;

        io.0.send(Cursor {
            op: op as i32,
            cursor: self.id,
            k: key.map(|v| v.to_vec()).unwrap_or_default(),
            v: value.map(|v| v.to_vec()).unwrap_or_default(),

            bucket_name: Default::default(),
        })
        .await?;

        let rsp = io.1.message().await?.context("no response")?;

        Ok((rsp.k.into(), rsp.v.into()))
    }
}

#[async_trait]
impl<'tx> traits::Cursor for RemoteCursor<'tx> {
    async fn first(&mut self) -> anyhow::Result<(Bytes, Bytes)> {
        self.op(Op::First, None, None).await
    }

    async fn seek(&mut self, key: &[u8]) -> anyhow::Result<(Bytes, Bytes)> {
        self.op(Op::Seek, Some(key), None).await
    }

    async fn seek_exact(&mut self, key: &[u8]) -> anyhow::Result<(Bytes, Bytes)> {
        self.op(Op::SeekExact, Some(key), None).await
    }

    async fn next(&mut self) -> anyhow::Result<(Bytes, Bytes)> {
        self.op(Op::Next, None, None).await
    }

    async fn prev(&mut self) -> anyhow::Result<(Bytes, Bytes)> {
        self.op(Op::Prev, None, None).await
    }

    async fn last(&mut self) -> anyhow::Result<(Bytes, Bytes)> {
        self.op(Op::Last, None, None).await
    }

    async fn current(&mut self) -> anyhow::Result<(Bytes, Bytes)> {
        self.op(Op::Current, None, None).await
    }
}

#[async_trait]
impl<'tx> traits::CursorDupSort for RemoteCursor<'tx> {
    async fn seek_both_exact(
        &mut self,
        key: &[u8],
        value: &[u8],
    ) -> anyhow::Result<(Bytes, Bytes)> {
        self.op(Op::SeekBothExact, Some(key), Some(value)).await
    }

    async fn seek_both_range(
        &mut self,
        key: &[u8],
        value: &[u8],
    ) -> anyhow::Result<(Bytes, Bytes)> {
        self.op(Op::SeekBoth, Some(key), Some(value)).await
    }

    async fn first_dup(&mut self) -> anyhow::Result<Bytes> {
        Ok(self.op(Op::FirstDup, None, None).await?.1)
    }
    async fn next_dup(&mut self) -> anyhow::Result<(Bytes, Bytes)> {
        self.op(Op::NextDup, None, None).await
    }
    async fn next_no_dup(&mut self) -> anyhow::Result<(Bytes, Bytes)> {
        self.op(Op::NextNoDup, None, None).await
    }
    async fn last_dup(&mut self, key: &[u8]) -> anyhow::Result<Bytes> {
        Ok(self.op(Op::LastDup, Some(key), None).await?.1)
    }
}

#[async_trait]
impl<'tx> traits::CursorDupFixed for RemoteCursor<'tx> {
    async fn get_multi(&mut self) -> anyhow::Result<Bytes> {
        Ok(self.op(Op::GetMultiple, None, None).await?.1)
    }

    async fn next_multi(&mut self) -> anyhow::Result<(Bytes, Bytes)> {
        self.op(Op::NextMultiple, None, None).await
    }
}

impl RemoteTransaction {
    pub async fn open<C>(mut client: KvClient<C>) -> anyhow::Result<Self>
    where
        C: GrpcService<BoxBody>,
        <C as GrpcService<BoxBody>>::ResponseBody: 'static,
        <<C as GrpcService<BoxBody>>::ResponseBody as HttpBody>::Error:
            Into<Box<(dyn std::error::Error + Send + Sync + 'static)>> + Send,
    {
        trace!("Opening transaction");
        let (sender, mut rx) = channel(1);
        let mut receiver = client
            .tx(stream! {
                // Just a dummy message, workaround for
                // https://github.com/hyperium/tonic/issues/515
                yield Cursor {
                    op: Op::Open as i32,
                    bucket_name: "DUMMY".into(),
                    cursor: Default::default(),
                    k: Default::default(),
                    v: Default::default(),
                };
                while let Some(v) = rx.recv().await {
                    yield v;
                }
            })
            .await?
            .into_inner();

        // https://github.com/hyperium/tonic/issues/515
        let cursor = receiver.message().await?.context("no response")?.cursor_id;

        sender
            .send(Cursor {
                op: Op::Close as i32,
                cursor,
                bucket_name: Default::default(),
                k: Default::default(),
                v: Default::default(),
            })
            .await?;

        let _ = receiver.try_next().await?;

        trace!("Acquired transaction receiver");

        Ok(Self {
            io: Arc::new(AsyncMutex::new((sender, receiver))),
        })
    }
}