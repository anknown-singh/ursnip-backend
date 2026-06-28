# Deploying ursnip-backend to Oracle Cloud VM

This guide covers deploying the ursnip-backend to an Oracle Cloud Infrastructure (OCI) Compute instance running Ubuntu/Oracle Linux.

## Prerequisites

- Oracle Cloud account with a Compute instance (ARM Ampere A1 or AMD E2.1.Micro for free tier)
- SSH access to the VM
- A domain name pointing to the VM's public IP (for HTTPS)
- PostgreSQL database (can run on the same VM or use a managed service)

## 1. Provision the VM

In the OCI Console:

1. Go to Compute → Instances → Create Instance
2. Choose image: **Ubuntu 22.04** (or Oracle Linux 8)
3. Shape: **VM.Standard.A1.Flex** (ARM, 4 OCPU / 24 GB RAM free tier) or **VM.Standard.E2.1.Micro** (AMD, always free)
4. Add your SSH public key
5. Under Networking, ensure a public subnet with internet gateway is selected
6. Create the instance

### Open Firewall Ports

In OCI Console → Networking → Virtual Cloud Networks → Security Lists, add ingress rules:

| Port | Protocol | Source | Purpose |
|------|----------|--------|---------|
| 22 | TCP | Your IP | SSH |
| 80 | TCP | 0.0.0.0/0 | HTTP (redirect to HTTPS) |
| 443 | TCP | 0.0.0.0/0 | HTTPS |

Also open ports on the OS firewall:

```bash
sudo iptables -I INPUT -p tcp --dport 80 -j ACCEPT
sudo iptables -I INPUT -p tcp --dport 443 -j ACCEPT
sudo netfilter-persistent save
```

## 2. Install Dependencies on the VM

```bash
ssh ubuntu@<VM_PUBLIC_IP>

# Update system
sudo apt update && sudo apt upgrade -y

# Install build tools (for compiling Rust on the VM, or skip if using cross-compilation)
sudo apt install -y build-essential pkg-config libssl-dev curl

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# Install PostgreSQL 16
sudo apt install -y postgresql-16 postgresql-client-16

# Start and enable PostgreSQL
sudo systemctl enable postgresql
sudo systemctl start postgresql
```

## 3. Configure PostgreSQL

```bash
# Switch to postgres user and create the database + role
sudo -u postgres psql <<EOF
CREATE USER ursnip WITH PASSWORD 'your-secure-db-password';
CREATE DATABASE ursnip OWNER ursnip;
GRANT ALL PRIVILEGES ON DATABASE ursnip TO ursnip;
EOF
```

For production, edit `/etc/postgresql/16/main/pg_hba.conf` to use `scram-sha-256` authentication and restrict access to localhost only.

## 4. Deploy the Application

### Option A: Build on the VM

```bash
# Clone your repository
git clone https://your-repo-url/ursnip-backend.git
cd ursnip-backend

# Build in release mode
cargo build --release

# The binary is at target/release/ursnip-backend
```

### Option B: Cross-compile and upload (faster for ARM)

On your Mac (if the VM is ARM/aarch64):

```bash
# Install cross-compilation target
rustup target add aarch64-unknown-linux-gnu

# Install linker (via Homebrew)
brew install messense/macos-cross-toolchains/aarch64-unknown-linux-gnu

# Build
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-unknown-linux-gnu-gcc \
  cargo build --release --target aarch64-unknown-linux-gnu

# Upload to VM
scp target/aarch64-unknown-linux-gnu/release/ursnip-backend ubuntu@<VM_IP>:/home/ubuntu/ursnip-backend/
scp -r migrations/ ubuntu@<VM_IP>:/home/ubuntu/ursnip-backend/
```

For AMD (x86_64) VMs:

```bash
rustup target add x86_64-unknown-linux-gnu
cargo build --release --target x86_64-unknown-linux-gnu
scp target/x86_64-unknown-linux-gnu/release/ursnip-backend ubuntu@<VM_IP>:/home/ubuntu/ursnip-backend/
```

## 5. Configure Environment

```bash
# On the VM
cd /home/ubuntu/ursnip-backend
cp .env.example .env
nano .env
```

Key production settings:

```env
DATABASE_URL=postgres://ursnip:your-secure-db-password@localhost:5432/ursnip
JWT_SECRET=<generate-with: openssl rand -hex 32>
PORT=8080
LOG_LEVEL=info

# OAuth (get from Google/GitHub developer console)
GOOGLE_CLIENT_ID=your-production-client-id
GOOGLE_CLIENT_SECRET=your-production-secret
GITHUB_CLIENT_ID=your-production-client-id
GITHUB_CLIENT_SECRET=your-production-secret
OAUTH_REDIRECT_BASE_URL=https://your-domain.com

# AI Provider
AI_PROVIDER_URL=https://your-ai-provider.com/v1/expand
AI_PROVIDER_KEY=your-ai-key

# Billing
BILLING_WEBHOOK_SECRET=your-billing-webhook-secret

# Email
EMAIL_PROVIDER=smtp
EMAIL_FROM_ADDRESS=noreply@your-domain.com
EMAIL_SMTP_HOST=smtp.your-provider.com
EMAIL_SMTP_PORT=587
EMAIL_SMTP_USER=your-smtp-user
EMAIL_SMTP_PASSWORD=your-smtp-password

# Admin seed (change password immediately after first deploy)
SEED_ADMIN_EMAIL=admin@your-domain.com
SEED_ADMIN_PASSWORD=initial-admin-password-change-me

# CORS
CORS_ALLOWED_ORIGINS=https://your-domain.com,https://app.your-domain.com

# Security
TRUSTED_PROXY_CIDRS=127.0.0.1/32
```

## 6. Create systemd Service

```bash
sudo nano /etc/systemd/system/ursnip-backend.service
```

```ini
[Unit]
Description=ursnip-backend API server
After=network.target postgresql.service
Requires=postgresql.service

[Service]
Type=simple
User=ubuntu
Group=ubuntu
WorkingDirectory=/home/ubuntu/ursnip-backend
ExecStart=/home/ubuntu/ursnip-backend/ursnip-backend
EnvironmentFile=/home/ubuntu/ursnip-backend/.env
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/home/ubuntu/ursnip-backend

# Resource limits
LimitNOFILE=65536
LimitNPROC=4096

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable ursnip-backend
sudo systemctl start ursnip-backend

# Check status
sudo systemctl status ursnip-backend

# View logs
sudo journalctl -u ursnip-backend -f
```

## 7. Set Up Nginx Reverse Proxy with HTTPS

```bash
sudo apt install -y nginx certbot python3-certbot-nginx
```

Create Nginx config:

```bash
sudo nano /etc/nginx/sites-available/ursnip-backend
```

```nginx
server {
    listen 80;
    server_name your-domain.com;
    return 301 https://$server_name$request_uri;
}

server {
    listen 443 ssl http2;
    server_name your-domain.com;

    # SSL (managed by certbot)
    ssl_certificate /etc/letsencrypt/live/your-domain.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/your-domain.com/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_ciphers HIGH:!aNULL:!MD5;

    # Proxy settings
    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # WebSocket support
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 86400s;
        proxy_send_timeout 86400s;
    }

    # Body size limit (match app's 10MB for sync routes)
    client_max_body_size 10m;
}
```

Enable the site and get SSL certificate:

```bash
sudo ln -s /etc/nginx/sites-available/ursnip-backend /etc/nginx/sites-enabled/
sudo rm /etc/nginx/sites-enabled/default
sudo nginx -t
sudo systemctl restart nginx

# Get Let's Encrypt certificate
sudo certbot --nginx -d your-domain.com
```

## 8. Update TRUSTED_PROXY_CIDRS

Since Nginx is the reverse proxy on localhost, update `.env`:

```env
TRUSTED_PROXY_CIDRS=127.0.0.1/32
```

Restart the service:

```bash
sudo systemctl restart ursnip-backend
```

## 9. Verify Deployment

```bash
# Health check
curl https://your-domain.com/health

# Readiness check
curl https://your-domain.com/ready

# Register a test user
curl -X POST https://your-domain.com/auth/register \
  -H "Content-Type: application/json" \
  -d '{"email":"test@example.com","password":"testpass123","client_type":"web"}'
```

## 10. Maintenance

### View logs

```bash
sudo journalctl -u ursnip-backend -f --no-pager
```

### Restart service

```bash
sudo systemctl restart ursnip-backend
```

### Deploy updates

```bash
# On your local machine: build new binary
cargo build --release --target aarch64-unknown-linux-gnu

# Upload
scp target/aarch64-unknown-linux-gnu/release/ursnip-backend ubuntu@<VM_IP>:/home/ubuntu/ursnip-backend/ursnip-backend.new

# On the VM: swap binaries with minimal downtime
ssh ubuntu@<VM_IP> <<'EOF'
cd /home/ubuntu/ursnip-backend
mv ursnip-backend ursnip-backend.old
mv ursnip-backend.new ursnip-backend
chmod +x ursnip-backend
sudo systemctl restart ursnip-backend
# Verify
sleep 2 && curl -s localhost:8080/health
EOF
```

### Database backups

```bash
# Add to crontab: daily backup at 2 AM
echo "0 2 * * * pg_dump -U ursnip ursnip | gzip > /home/ubuntu/backups/ursnip-\$(date +\%Y\%m\%d).sql.gz" | crontab -
mkdir -p /home/ubuntu/backups
```

### SSL certificate renewal

Certbot auto-renews via systemd timer. Verify:

```bash
sudo certbot renew --dry-run
```

## Security Checklist

- [ ] Change `SEED_ADMIN_PASSWORD` after first login
- [ ] Set strong `JWT_SECRET` (minimum 32 bytes random)
- [ ] Restrict PostgreSQL to localhost only (`pg_hba.conf`)
- [ ] Enable `ufw` or `iptables` — only allow 22, 80, 443
- [ ] Set up unattended-upgrades for security patches
- [ ] Configure log rotation for journald
- [ ] Set up monitoring (health endpoint polling)
- [ ] Back up the database regularly
- [ ] Keep `.env` file permissions restricted: `chmod 600 .env`
