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
        .enumerate()
        .map(|(index, server)| {
            (
                index,
                Box::pin(upstream::exchange(server, query, client_protocol.clone())),
            )
        })
        .collect();
    let mut errors: Vec<_> = upstreams.iter().map(|_| None).collect();
    let mut last_error_kind = None;
    poll_fn(|cx| {
        let mut index = 0;
        while index < pending.len() {
            match pending[index].1.as_mut().poll(cx) {
                Poll::Ready(Ok(response)) => return Poll::Ready(Ok(response)),
                Poll::Ready(Err(error)) => {
                    last_error_kind = Some(error.kind());
                    errors[pending[index].0] = Some(error);
                    drop(pending.swap_remove(index));
                }
                Poll::Pending => index += 1,
            }
        }
        if pending.is_empty() {
            let failures: Vec<_> = upstreams
                .iter()
                .zip(&errors)
                .map(|(server, error)| {
                    let protocol = match server.protocol.as_ref().unwrap_or(&client_protocol) {
                        Protocol::Tcp => "tcp",
                        Protocol::Udp => "udp",
                        Protocol::Tls => "tls",
                    };
                    let address = std::net::SocketAddr::new(server.ip, server.port.unwrap_or(53));
                    let error = error.as_ref().expect("all upstreams failed");
                    format!("{protocol}://{address}: {error}")
                })
                .collect();
            Poll::Ready(Err(io::Error::new(
                last_error_kind.expect("nonempty group completed"),
                format!("no upstream answered: {}", failures.join("; ")),
            )))
        } else {
            Poll::Pending
        }
    })
    .await
}
