mod bin;
pub use bin::*;

mod command;
pub use command::*;

use anyhow::Result;

pub fn encode_packet(payload: &impl BinaryData, vec: &mut Vec<u8>) {
    BinaryWriter::new(vec).write(payload).unwrap();
}

pub fn decode_packet<T>(data: &[u8]) -> Result<T>
where
    T: BinaryData,
{
    BinaryReader::new(data).read()
}

// ── Stream and related utilities (requires tokio, not available in WASM) ──────

#[cfg(feature = "stream")]
mod stream_impl {
    use crate::{decode_packet, encode_packet, BinaryData};
    use anyhow::{anyhow, bail, Error, Result};
    use argon2::{password_hash::SaltString, Argon2, PasswordHasher};
    use std::{future::Future, marker::PhantomData, sync::Arc, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        sync::{mpsc, oneshot},
        task::JoinHandle,
    };
    use tracing::{error, trace, warn};

    pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);
    pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(2);
    pub const HEARTBEAT_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(10);

    struct Outbound<S> {
        payload: S,
        flushed: Option<oneshot::Sender<std::result::Result<(), String>>>,
    }

    pub struct StreamSender<S> {
        tx: mpsc::Sender<Outbound<S>>,
    }

    impl<S> StreamSender<S> {
        pub async fn send(&self, payload: S) -> Result<()> {
            self.tx
                .send(Outbound {
                    payload,
                    flushed: None,
                })
                .await
                .map_err(|_| anyhow!("send queue closed"))
        }

        pub async fn send_and_flush(&self, payload: S) -> Result<()> {
            let (flushed_tx, flushed_rx) = oneshot::channel();
            self.tx
                .send(Outbound {
                    payload,
                    flushed: Some(flushed_tx),
                })
                .await
                .map_err(|_| anyhow!("send queue closed"))?;

            match flushed_rx.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(message)) => Err(anyhow!(message)),
                Err(_) => Err(anyhow!("send task stopped before flushing packet")),
            }
        }

        pub fn blocking_send(&self, payload: S) -> Result<()> {
            self.tx
                .blocking_send(Outbound {
                    payload,
                    flushed: None,
                })
                .map_err(|_| anyhow!("send queue closed"))
        }
    }

    pub struct Stream<S, R> {
        version: u8,

        send_tx: Arc<StreamSender<S>>,

        send_task_handle: JoinHandle<()>,
        recv_task_handle: JoinHandle<Result<()>>,

        _marker: PhantomData<(S, R)>,
    }

    impl<S, R> Stream<S, R>
    where
        S: BinaryData + std::fmt::Debug + Send + Sync + 'static,
        R: BinaryData + std::fmt::Debug + Send + 'static,
    {
        pub async fn new<F>(
            version: Option<u8>,
            stream: TcpStream,
            mut handler: Box<dyn FnMut(Arc<StreamSender<S>>, R) -> F + Send + Sync>,
        ) -> Result<Self>
        where
            F: Future<Output = ()> + Send + 'static,
        {
            stream.set_nodelay(true)?;
            let (mut read, mut write) = stream.into_split();
            let version = if let Some(version) = version {
                write.write_u8(version).await?;
                version
            } else {
                read.read_u8().await?
            };

            let (send_tx, mut send_rx) = mpsc::channel(1024);
            let send_tx = Arc::new(StreamSender { tx: send_tx });
            let send_task_handle = tokio::spawn({
                async move {
                    let mut buffer = Vec::new();
                    let mut len_buf = [0u8; 5];
                    while let Some(outbound) = send_rx.recv().await {
                        let Outbound { payload, flushed } = outbound;
                        buffer.clear();
                        encode_packet(&payload, &mut buffer);
                        trace!("sending {} bytes ({payload:?}): {buffer:?}", buffer.len());

                        let mut x = buffer.len() as u32;
                        let mut n = 0;
                        loop {
                            len_buf[n] = (x & 0x7f) as u8;
                            n += 1;
                            x >>= 7;
                            if x == 0 {
                                break;
                            } else {
                                len_buf[n - 1] |= 0x80;
                            }
                        }

                        let result = async {
                            write.write_all(&len_buf[..n]).await?;
                            write.write_all(&buffer).await?;
                            write.flush().await?;
                            Ok::<_, Error>(())
                        }
                        .await;

                        if let Some(flushed) = flushed {
                            let status = result.as_ref().map(|_| ()).map_err(|err| err.to_string());
                            let _ = flushed.send(status);
                        }

                        if let Err(err) = result {
                            error!("failed to send: {err:?}");
                            break;
                        }
                    }
                }
            });

            let recv_task_handle = tokio::spawn({
                let send_tx = Arc::clone(&send_tx);
                #[allow(clippy::read_zero_byte_vec)]
                async move {
                    let mut buffer = Vec::new();
                    loop {
                        let mut len = 0u32;
                        let mut pos = 0;
                        loop {
                            let byte = read.read_u8().await?;
                            len |= ((byte & 0x7f) as u32) << pos;
                            pos += 7;
                            if byte & 0x80 == 0 {
                                break;
                            }
                            if pos > 32 {
                                bail!("invalid length");
                            }
                        }
                        if len > 2 * 1024 * 1024 {
                            bail!("data packet too large");
                        }
                        let len = len as usize;

                        buffer.resize(len, 0);
                        read.read_exact(&mut buffer).await?;
                        trace!("received {} bytes: {buffer:?}", buffer.len());

                        let payload: R = match decode_packet(&buffer) {
                            Ok(val) => val,
                            Err(err) => {
                                warn!("invalid packet: {err:?} {buffer:?}");
                                break;
                            }
                        };
                        trace!("decodes to {payload:?}");
                        handler(Arc::clone(&send_tx), payload).await;
                    }
                    Ok(())
                }
            });

            Ok(Self {
                version,

                send_tx,

                send_task_handle,
                recv_task_handle,

                _marker: PhantomData,
            })
        }

        pub fn version(&self) -> u8 {
            self.version
        }

        pub async fn send(&self, payload: S) -> Result<()> {
            self.send_tx.send(payload).await
        }

        pub async fn send_and_flush(&self, payload: S) -> Result<()> {
            self.send_tx.send_and_flush(payload).await
        }

        pub fn blocking_send(&self, payload: S) -> Result<()> {
            self.send_tx.blocking_send(payload)
        }
        /// Abort both socket tasks immediately. This is used when a session is
        /// replaced, kicked, or banned; dropping the outer Arc may be delayed by
        /// task-owned references.
        pub fn close(&self) {
            self.send_task_handle.abort();
            self.recv_task_handle.abort();
        }
    }

    impl<S, R> Drop for Stream<S, R> {
        fn drop(&mut self) {
            self.send_task_handle.abort();
            self.recv_task_handle.abort();
        }
    }

    pub fn generate_secret_key(info: &str, len: usize) -> Result<Vec<u8>> {
        let original = std::env::var("HSN_SECRET_KEY").unwrap_or_else(|_| {
            warn!("HSN_SECRET_KEY is not set, using default value");
            "some_random_secret_key_for_debugging".to_string()
        });
        let salt = SaltString::encode_b64(b"some$random#salt")
            .map_err(|e| anyhow!("failed to generate salt string: {e}"))?;
        let ikm = Argon2::default()
            .hash_password(original.as_bytes(), &salt)
            .map_err(|e| anyhow!("error calculating hash: {e}"))?
            .hash
            .ok_or_else(|| anyhow!("error calculating hash"))?;

        let h = hkdf::Hkdf::<sha2::Sha256>::new(None, ikm.as_ref());
        let mut okm = vec![0u8; len];
        h.expand(info.as_bytes(), &mut okm)?;
        Ok(okm)
    }
}

#[cfg(feature = "stream")]
pub use stream_impl::*;
