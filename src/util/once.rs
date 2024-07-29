use std::fmt::Formatter;
use std::pin::Pin;
use std::task::{Context, Poll};
use hyper::body::{Buf, Bytes, Frame};

pub struct Once(Bytes);

unsafe impl Send for Once {}
unsafe impl Sync for Once {}
impl Unpin for Once {}

impl From<Bytes> for Once {
    #[inline]
    fn from(chunk: Bytes) -> Self {
        Self(chunk)
    }
}
impl From<Vec<u8>> for Once {
    #[inline]
    fn from(vec: Vec<u8>) -> Self {
        Self::from(Bytes::from(vec))
    }
}

impl From<String> for Once {
    #[inline]
    fn from(s: String) -> Self {
        Self::from(Bytes::from(s.into_bytes()))
    }
}

impl From<&'static str> for Once {
    #[inline]
    fn from(s: &'static str) -> Self {
        Self::from(Bytes::from(s))
    }
}

impl Buf for Once {
    fn remaining(&self) -> usize {
        self.0.remaining()
    }

    fn chunk(&self) -> &[u8] {
        self.0.chunk()
    }

    fn advance(&mut self, cnt: usize) {
        self.0.advance(cnt);
    }
}

#[derive(Debug)]
pub struct OnceError;

unsafe impl Send for OnceError {}
unsafe impl Sync for OnceError {}
impl std::fmt::Display for OnceError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("OnceError")
    }
}

impl std::error::Error for OnceError {}

impl hyper::body::Body for Once {
    type Data = Bytes;
    type Error = OnceError;

    fn poll_frame(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(Some(Ok(Frame::data(self.0.clone()))))
    }
}
