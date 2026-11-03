use reqwest::Response as ReqwestResponse;
use ruffle_core::backend::navigator::{OwnedFuture, SuccessResponse};
use ruffle_core::loader::Error;
use ruffle_core::swf::Encoding;
use std::sync::{Arc, Mutex};

pub enum ResponseBody {
    /// The response's body comes from a file.
    File(Result<Vec<u8>, std::io::Error>),

    /// The response's body comes from the network.
    ///
    /// This has to be stored in shared ownership so that we can return
    /// owned futures. A synchronous lock is used here as we do not
    /// expect contention on this lock.
    Network(Arc<Mutex<Option<ReqwestResponse>>>),
}

pub struct Response {
    pub url: String,
    pub response_body: ResponseBody,
    pub text_encoding: Option<&'static Encoding>,
    pub status: u16,
    pub redirected: bool,
}

impl SuccessResponse for Response {
    fn url(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed(&self.url)
    }

    fn set_url(&mut self, url: String) {
        self.url = url;
    }

    fn body(self: Box<Self>) -> OwnedFuture<Vec<u8>, Error> {
        match self.response_body {
            ResponseBody::File(file) => {
                Box::pin(async move { file.map_err(|e| Error::FetchError(e.to_string())) })
            }
            ResponseBody::Network(response) => Box::pin(async move {
                let resp = response
                    .lock()
                    .expect("working lock during fetch body read")
                    .take()
                    .expect("Body cannot already be consumed");
                crate::backends::navigator::spawn_tokio(async move {
                    resp.bytes()
                        .await
                        .map(|b| b.to_vec())
                        .map_err(|e| e.to_string())
                })
                .await
                .map_err(Error::FetchError)
            }),
        }
    }

    fn text_encoding(&self) -> Option<&'static Encoding> {
        self.text_encoding
    }

    fn status(&self) -> u16 {
        self.status
    }

    fn redirected(&self) -> bool {
        self.redirected
    }

    fn next_chunk(&mut self) -> OwnedFuture<Option<Vec<u8>>, Error> {
        match &mut self.response_body {
            ResponseBody::File(file) => {
                let res = file
                    .as_mut()
                    .map(std::mem::take)
                    .map_err(|e| Error::FetchError(e.to_string()));

                Box::pin(async move {
                    match res {
                        Ok(bytes) if !bytes.is_empty() => Ok(Some(bytes)),
                        Ok(_) => Ok(None),
                        Err(e) => Err(e),
                    }
                })
            }
            ResponseBody::Network(response) => {
                let response = response.clone();
                Box::pin(async move {
                    crate::backends::navigator::spawn_tokio(async move {
                        let mut resp = {
                            let mut lock = match response.try_lock() {
                                Ok(lock) => lock,
                                Err(std::sync::TryLockError::WouldBlock) => {
                                    return Err(
                                        "Concurrent read operations on the same stream are not supported."
                                            .to_string(),
                                    );
                                }
                                Err(_) => panic!("desktop network lock"),
                            };
                            match lock.take() {
                                Some(resp) => resp,
                                None => {
                                    return Err(
                                        "Concurrent read operations on the same stream are not supported."
                                            .to_string(),
                                    );
                                }
                            }
                        };

                        let mut buf = Vec::new();
                        let result = loop {
                            match resp.chunk().await {
                                Ok(Some(bytes)) => {
                                    buf.extend_from_slice(&bytes);
                                    if buf.len() >= 1024 * 1024 {
                                        break Ok(Some(buf));
                                    }
                                }
                                Ok(None) => {
                                    if buf.is_empty() {
                                        break Ok(None);
                                    } else {
                                        break Ok(Some(buf));
                                    }
                                }
                                Err(e) => break Err(e.to_string()),
                            }
                        };

                        if let Ok(mut lock) = response.lock() {
                            *lock = Some(resp);
                        }
                        result
                    })
                    .await
                    .map_err(Error::FetchError)
                })
            }
        }
    }

    fn expected_length(&self) -> Result<Option<u64>, Error> {
        match &self.response_body {
            ResponseBody::File(file) => Ok(file.as_ref().map(|file| file.len() as u64).ok()),
            ResponseBody::Network(response) => {
                let lock = response.lock().expect("no recursive locks");
                let response = lock.as_ref().expect("Body cannot already be consumed");
                Ok(response.content_length())
            }
        }
    }
}
