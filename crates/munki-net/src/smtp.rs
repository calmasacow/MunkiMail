use anyhow::{Context, Result};
use mail_send::{Credentials, SmtpClient, SmtpClientBuilder};
use munki_core::{AccountId, OpKind};
use munki_store::Store;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

/// Verified implicit TLS or required STARTTLS; credentials sent after TLS.
pub async fn connect(
    host: &str,
    port: u16,
    implicit_tls: bool,
    credentials: Credentials<String>,
) -> Result<SmtpClient<TlsStream<TcpStream>>> {
    // Standard submission ports determine TLS negotiation, including older saved settings.
    let implicit_tls = match port {
        465 => true,
        587 => false,
        _ => implicit_tls,
    };
    let builder = SmtpClientBuilder::new(host.to_string(), port).map_err(anyhow::Error::msg)?;
    builder
        .implicit_tls(implicit_tls)
        .credentials(credentials)
        .connect()
        .await.with_context(||format!("SMTP connection to {host}:{port} failed using {}. Port 465 normally uses implicit TLS; port 587 uses STARTTLS",if implicit_tls {"implicit TLS"}else{"STARTTLS"}))
}

/// Unknown outcomes are parked. Explicit 4xx replies are safe to retry.
pub async fn drain_outbox<S: AsyncRead + AsyncWrite + Unpin>(
    client: &mut SmtpClient<S>,
    store: &mut Store,
    account: AccountId,
) -> Result<crate::imap::DrainReport> {
    store.recover_leases(account)?;
    let mut report = crate::imap::DrainReport::default();
    while let Some(op) = store.lease_transport(account, true)? {
        let OpKind::Submit { draft_id } = op.kind else {
            unreachable!()
        };
        let draft = store.draft(account, draft_id)?;
        let (_, wire) = store.draft_wire(account, draft_id)?;
        let recipients = draft
            .to
            .iter()
            .chain(&draft.cc)
            .chain(&draft.bcc)
            .map(String::as_str);
        let message = mail_send::smtp::message::Message::new(
            draft.from.as_str(),
            recipients,
            wire.as_slice(),
        );
        match client.send(message).await {
            Ok(_) => {
                store.delivered(account, op.id, draft_id)?;
                report.applied += 1;
            }
            Err(error) => {
                let retry = matches!(&error,mail_send::Error::UnexpectedReply(reply) if reply.code / 100 == 4);
                if retry {
                    store.settle_retry(op.id, &error.to_string())?;
                    report.retried += 1;
                } else {
                    store.settle_needs_review(
                        op.id,
                        &format!("delivery failed or uncertain; review before resending: {error}"),
                    )?;
                    report.parked += 1;
                }
                break;
            }
        }
    }
    Ok(report)
}
