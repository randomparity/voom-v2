# Runbook: Control-Plane API Server

Run the control-plane API as a foreground HTTPS process. The server opens an
existing VOOM database and never creates or migrates one.

## Prerequisites

Initialize the database once with the CLI before starting the server:

```sh
export VOOM_DATABASE_URL=sqlite:///var/lib/voom/voom.db
voom init
```

Provision a CA-issued server certificate whose subject alternative names include
every DNS name or IP address clients use for the configured bind. Install the
certificate chain and matching private key so only the service account can read
the key, for example:

```sh
sudo install -o voom -g voom -m 0644 server-chain.pem /etc/voom/api-chain.pem
sudo install -o voom -g voom -m 0600 server-key.pem /etc/voom/api-key.pem
```

## Start HTTPS

Run the process under a supervisor that forwards SIGTERM and captures stderr:

```sh
export VOOM_DATABASE_URL=sqlite:///var/lib/voom/voom.db
voom-api \
  --bind 0.0.0.0:7443 \
  --tls-cert /etc/voom/api-chain.pem \
  --tls-key /etc/voom/api-key.pem
```

The process writes structured JSON diagnostics to stderr and nothing to stdout.
It is ready after emitting a record with `"event":"listening"`. Probe readiness
through the CA that issued the server certificate:

```sh
curl --fail --silent --show-error \
  --cacert /etc/voom/client-ca.pem \
  https://api.example.test:7443/health
```

Authenticated execution routes use a node bearer token. Read it without echo,
send it only over verified HTTPS, and clear it from the shell afterward. This
heartbeat example is a mutation, so it also supplies an idempotency key:

```bash
read -r -s -p 'Node bearer token: ' VOOM_NODE_TOKEN
printf '\n' >&2
curl --fail --silent --show-error \
  --cacert /etc/voom/client-ca.pem \
  --header "Authorization: Bearer ${VOOM_NODE_TOKEN}" \
  --header 'Content-Type: application/json' \
  --header 'X-Voom-Idempotency-Key: operator-heartbeat-001' \
  --data '{}' \
  "https://api.example.test:7443/v1/execution/node/${VOOM_NODE_ID}/heartbeat"
unset VOOM_NODE_TOKEN
```

## Loopback-only HTTP

Cleartext is available only when explicitly enabled on an IPv4 or IPv6 loopback
bind. It is intended for local development and probes:

```sh
export VOOM_DATABASE_URL=sqlite:///var/lib/voom/voom.db
voom-api --bind 127.0.0.1:7443 --allow-cleartext-loopback
```

There is no cleartext option for remote traffic. Configure both TLS paths for a
non-loopback bind.

## Shutdown, retries, and certificate renewal

Send SIGTERM (or SIGINT) to begin shutdown. The listener stops accepting new
connections, in-flight requests receive up to 30 seconds to finish, and stderr
then records `"event":"shutdown_complete"` before a successful exit. Configure
the service supervisor's stop timeout above 30 seconds so it does not send
SIGKILL during the drain.

A transport cutoff or `REQUEST_TIMEOUT` (`HTTP 408`) does not prove that a
mutation failed: its durable commit may have completed before the response was
lost. Retry the same method, path, body, authorization scope, and
`X-Voom-Idempotency-Key` to replay the original result without repeating the
mutation. Generate a new key only for a new logical operation.

The process loads its certificate and key only at startup. After renewal,
restart it with SIGTERM-driven draining so the new identity is loaded.

## Troubleshooting

| Symptom | Operator action |
|---|---|
| `cleartext requires a loopback --bind` | Use `127.0.0.1` or `::1` with `--allow-cleartext-loopback`, or configure both TLS paths. |
| Transport-selection error with one TLS path | Supply both `--tls-cert` and `--tls-key`; a partial pair is rejected. Do not combine them with `--allow-cleartext-loopback`. |
| TLS identity fails to load | Check certificate/key read permissions, PEM encoding, certificate-chain order, and that the private key matches. Server diagnostics intentionally omit path values and key details. |
| Client reports an expired certificate | Renew the certificate, verify its validity interval, and restart `voom-api`. |
| Client reports an unknown or untrusted issuer | Install the issuing CA in the client trust store or pass the correct CA with `curl --cacert`; never disable verification. |
| `HTTP 408` / `REQUEST_TIMEOUT` | Correct the slow dependency or retry the identical mutation with the same idempotency key. |
| `HTTP 413` / `PAYLOAD_TOO_LARGE` | Reduce the request body below the fixed 1 MiB limit; do not split one logical mutation across reused idempotency keys. |
| Server reports the database is unreachable or uninitialized | Verify `VOOM_DATABASE_URL`, file ownership, and that `voom init` completed. The server does not create directories, database files, or migrations. |
