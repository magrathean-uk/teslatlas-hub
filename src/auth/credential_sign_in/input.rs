// SPDX-License-Identifier: AGPL-3.0-only

use tokio::{io::{AsyncRead, AsyncReadExt}, sync::watch, time::Instant};
use zeroize::{Zeroize, Zeroizing};
use super::observe::{SecretBuffer, SecretDropObserver, current_secret_observer, note_secret_drop};

pub const SIGN_IN_LINE_CAP: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInInputError { TooLong, TimedOut, Closed, Cancelled, Read }

/// One bounded reader owns every input byte for both sign-in lines.
/// Dropping it wipes any bytes following the last line the command consumed.
pub struct SignInInput<R> {
    reader: R,
    pending: SecretBuffer,
    scratch: Zeroizing<[u8; 1024]>,
    observer: Option<SecretDropObserver>,
}

impl<R: AsyncRead + Unpin> SignInInput<R> {
    pub fn new(reader: R) -> Self {
        Self { reader, pending: SecretBuffer::new("input_pending", SIGN_IN_LINE_CAP), scratch: Zeroizing::new([0; 1024]), observer: current_secret_observer() }
    }

    pub async fn line(
        &mut self, deadline: Instant, cancellation: &mut watch::Receiver<bool>,
    ) -> Result<SecretBuffer, SignInInputError> {
        loop {
            if *cancellation.borrow() { return Err(SignInInputError::Cancelled); }
            if Instant::now() >= deadline { return Err(SignInInputError::TimedOut); }
            if let Some(index) = self.pending.iter().position(|byte| *byte == b'\n') {
                if index >= SIGN_IN_LINE_CAP { return Err(SignInInputError::TooLong); }
                let line = SecretBuffer::copy("input_line", &self.pending[..index]);
                let remainder = SecretBuffer::copy("input_tail", &self.pending[index + 1..]);
                self.pending.zeroize();
                self.pending.extend_from_slice(&remainder);
                return Ok(line);
            }
            if self.pending.len() >= SIGN_IN_LINE_CAP { return Err(SignInInputError::TooLong); }
            let limit = self.scratch.len().min(SIGN_IN_LINE_CAP - self.pending.len());
            let read = tokio::select! {
                biased;
                changed = cancellation.changed() => {
                    if changed.is_err() || *cancellation.borrow() { return Err(SignInInputError::Cancelled); }
                    continue;
                }
                _ = tokio::time::sleep_until(deadline) => return Err(SignInInputError::TimedOut),
                read = self.reader.read(&mut self.scratch[..limit]) => read.map_err(|_| SignInInputError::Read)?,
            };
            if read == 0 { return Err(SignInInputError::Closed); }
            self.pending.extend_from_slice(&self.scratch[..read]);
            self.scratch.zeroize();
        }
    }
}

impl<R> Drop for SignInInput<R> {
    fn drop(&mut self) {
        self.scratch.zeroize();
        note_secret_drop(&self.observer, "input_scratch", 1024, 1024, self.scratch.iter().all(|byte| *byte == 0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn one_reader_retains_the_second_line_and_discards_the_tail_on_release() {
        let (_send, mut cancellation) = watch::channel(false);
        let mut reader = SignInInput::new(&b"settings\nanswer\ntrailing"[..]);
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(&*reader.line(deadline, &mut cancellation).await.expect("settings"), b"settings");
        assert_eq!(&*reader.line(deadline, &mut cancellation).await.expect("answer"), b"answer");
        assert_eq!(&*reader.pending, b"trailing");
    }

    #[tokio::test]
    async fn line_cap_deadline_eof_and_cancellation_refuse_without_waiting_for_more() {
        let (_send, mut cancellation) = watch::channel(false);
        let bytes = vec![b'x'; SIGN_IN_LINE_CAP];
        let mut capped = SignInInput::new(bytes.as_slice());
        assert_eq!(capped.line(Instant::now() + Duration::from_secs(1), &mut cancellation).await.unwrap_err(), SignInInputError::TooLong);
        let mut eof = SignInInput::new(&b"incomplete"[..]);
        assert_eq!(eof.line(Instant::now() + Duration::from_secs(1), &mut cancellation).await.unwrap_err(), SignInInputError::Closed);
        let (mut sender, receiver) = tokio::io::duplex(64);
        sender.write_all(b"partial").await.expect("partial line");
        let mut reader = SignInInput::new(receiver);
        assert_eq!(reader.line(Instant::now(), &mut cancellation).await.unwrap_err(), SignInInputError::TimedOut);
        let (send, mut cancellation) = watch::channel(false);
        send.send(true).expect("cancel");
        assert_eq!(reader.line(Instant::now() + Duration::from_secs(1), &mut cancellation).await.unwrap_err(), SignInInputError::Cancelled);
    }
}
