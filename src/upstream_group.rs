use crate::config::{Protocol, Upstream};
use crate::upstream;
use std::future::{Future, poll_fn};
use std::io;
use std::task::Poll;

/// Race one exchange per upstream, preserving the single-upstream validation,
/// protocol selection and timeout. Dropping this future also cancels the group.
///
/// Exchanges are polled in this task (not spawned): returning the first valid
/// response drops every remaining future and its socket/connection immediately.
pub async fn exchange(
    upstreams: &[Upstream],
    query: &[u8],
    client_protocol: Protocol,
) -> io::Result<Vec<u8>> {
    if upstreams.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty upstream group",
        ));
    }
    let mut pending: Vec<_> = upstreams
        .iter()
        .map(|server| Box::pin(upstream::exchange(server, query, client_protocol.clone())))
        .collect();
    let mut last_error = None;
    poll_fn(|cx| {
        let mut index = 0;
        while index < pending.len() {
            match pending[index].as_mut().poll(cx) {
                Poll::Ready(Ok(response)) => return Poll::Ready(Ok(response)),
                Poll::Ready(Err(error)) => {
                    last_error = Some(error);
                    drop(pending.swap_remove(index));
                }
                Poll::Pending => index += 1,
            }
        }
        if pending.is_empty() {
            Poll::Ready(Err(last_error.take().expect("nonempty group completed")))
        } else {
            Poll::Pending
        }
    })
    .await
}
