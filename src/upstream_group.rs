use crate::config::{Protocol, Upstream};
use crate::dns;
use crate::upstream;
use std::future::{Future, poll_fn};
use std::io;
use std::task::Poll;

fn is_usable_reply(response: &[u8]) -> bool {
    matches!(dns::rcode(response), Ok(0 | 3))
}

/// Race one exchange per upstream, preserving the single-upstream validation,
/// protocol selection and timeout. Dropping this future also cancels the group.
///
/// Exchanges are polled in this task (not spawned): returning the first usable
/// response (NOERROR or NXDOMAIN) drops every remaining future and its
/// socket/connection immediately. Validated DNS error replies are retained as
/// fallback so a fast SERVFAIL/REFUSED/extended error does not mask a slower
/// usable reply.
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
    let mut fallback_response = None;
    poll_fn(|cx| {
        let mut index = 0;
        while index < pending.len() {
            match pending[index].1.as_mut().poll(cx) {
                Poll::Ready(Ok(response)) => {
                    drop(pending.swap_remove(index));
                    if is_usable_reply(&response) {
                        return Poll::Ready(Ok(response));
                    }
                    fallback_response.get_or_insert(response);
                }
                Poll::Ready(Err(error)) => {
                    last_error_kind = Some(error.kind());
                    errors[pending[index].0] = Some(error);
                    drop(pending.swap_remove(index));
                }
                Poll::Pending => index += 1,
            }
        }
        if pending.is_empty() {
            if let Some(response) = fallback_response.take() {
                return Poll::Ready(Ok(response));
            }
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
