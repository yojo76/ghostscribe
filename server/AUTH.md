# Enabling shared-secret auth (deferred)

Auth is currently **off**. Anyone who can reach port 5005 can transcribe.
When we're ready to lock it down, follow these steps.

## 1. Generate a token

```bash
python3 -c "import secrets; print(secrets.token_urlsafe(32))"
```

Treat the output like a password. Do not commit it.

## 2. Enable on the server

```bash
sudo sh -c 'printf "\n# Shared-secret auth. Clients send X-Auth-Token: <value>\nGHOSTSCRIBE_AUTH_TOKEN=PASTE_TOKEN_HERE\n" >> /etc/ghostscribe/server.env'
sudo systemctl restart ghostscribe-server
```

Verify it came up with auth on:

```bash
journalctl -u ghostscribe-server -n 20 --no-pager | grep -i auth
# expect: auth=on
```

## 3. Verify enforcement

```bash
arecord -d 2 -f S16_LE -r 16000 -c 1 /tmp/t.wav

# No token -> expect HTTP 401
curl -i -F "audio=@/tmp/t.wav" http://localhost:5005/v1/auto

# With token -> expect JSON transcript
curl -i -H "X-Auth-Token: PASTE_TOKEN_HERE" \
     -F "audio=@/tmp/t.wav" http://localhost:5005/v1/auto
```

## 4. Update the Linux client

Edit `client/linux/config.toml` (not the example) and set:

```toml
auth_token = "PASTE_TOKEN_HERE"
```

Or pass `--auth-token ...` on the CLI. The client sends it as `X-Auth-Token`
on every request automatically.

## 5. Hand-off for colleagues (through the Windows SSH tunnel)

```bash
curl -H "X-Auth-Token: PASTE_TOKEN_HERE" \
     -F "audio=@sample.wav" \
     http://<windows-vpn-ip>:5005/v1/auto
```

## Where the token lives

- Server: `/etc/ghostscribe/server.env` (root-owned, outside the repo)
- Client: `client/linux/config.toml` (git-ignored; only
  `config.example.toml` is committed)

If the token ever leaks, generate a new one, overwrite both files,
`sudo systemctl restart ghostscribe-server`.
