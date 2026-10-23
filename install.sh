#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 PDMS contributors
# PDMS installer.
#
#   ./install.sh                     first install, or finish anything not yet set up
#   ./install.sh --configure network redo one part later (repeatable, comma-separated):
#                                    network, database-port, source, ocr, clamav,
#                                    admin, move-settings
#   ./install.sh --non-interactive   take answers from PDMS_INSTALL_* variables (see --help)
#
# What it does: checks prerequisites, finds free ports, asks how users will
# reach the system (bundled Caddy with automatic HTTPS, your own reverse
# proxy, or local testing), generates strong random secrets (database
# passwords, the master key), asks whether to include the optional OCR and
# virus-scanning containers, then builds and starts everything and creates
# the first administrator. Microsoft sign-in, email, SharePoint, the Entra ID
# user sync, OCR and virus scanning are then set up under Administration.
#
# Re-running is safe: nothing already set up is changed or asked again, and
# secrets are never regenerated. Secrets live in secrets/ (one file each,
# mounted as Docker secrets), never in .env or config/pdms.toml.
#   .env               deployment settings for Docker Compose (ports, profiles)
#   config/pdms.toml   application settings; blocks marked "install.sh:<name>"
#                      are managed here, everything else is yours to edit
#   secrets/           passwords and keys (keep private, back up securely)

set -euo pipefail
cd "$(dirname "$0")"

DEFAULT_NAME="Policy and Document Management System"
UPLOAD_LIMIT="110MB" # the app's 100 MB upload limit plus form overhead

# --- arguments ----------------------------------------------------------------
NON_INTERACTIVE=false
CONFIGURE=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --non-interactive) NON_INTERACTIVE=true ;;
    --configure) shift; CONFIGURE="${1:-}" ;;
    --configure=*) CONFIGURE="${1#*=}" ;;
    -h|--help)
      sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'
      cat <<'EOF'

Unattended installs (--non-interactive) read these variables; anything unset
takes its default or is skipped:
  PDMS_INSTALL_NAME, PDMS_INSTALL_TIMEZONE, PDMS_INSTALL_SOURCE_URL (default: the git remote)
  PDMS_INSTALL_PROXY        caddy | external | local   (default: local)
  PDMS_INSTALL_PORT         host port for the app      (default: first free from 8080)
  PDMS_INSTALL_CADDY_HTTP_PORT, PDMS_INSTALL_CADDY_HTTPS_PORT
                            Caddy's host ports (default: 80 and 443, or the next free ones)
  PDMS_INSTALL_DOMAIN       domain for Caddy (blank: this host's IP with an internal certificate)
  PDMS_INSTALL_PUBLIC_URL   the URL users open (external proxy)
  PDMS_INSTALL_PROXY_IP     your reverse proxy's IP address (external proxy)
  PDMS_INSTALL_DB_PORT      expose PostgreSQL on 127.0.0.1 at this port, or "yes" for the first free one (blank: don't)
  PDMS_INSTALL_ADMIN_EMAIL, PDMS_INSTALL_ADMIN_USERNAME (default: sysadmin), PDMS_INSTALL_ADMIN_NAME,
  PDMS_INSTALL_ADMIN_PASSWORD (blank: generated)
  PDMS_INSTALL_OCR, PDMS_INSTALL_CLAMAV   include the OCR / virus-scanning container: yes | no (default: no)
  PDMS_INSTALL_MOVE_SETTINGS              an upgrade moves the settings an earlier installer wrote
                                          into Administration: yes | no (default: yes)
EOF
      exit 0 ;;
    *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done
configuring() { [[ ",$CONFIGURE," == *",$1,"* ]]; }

# These steps moved into Administration (S1): say where, instead of asking.
moved_to() {
  case "$1" in
    sso)        echo "Microsoft sign-in: Administration -> Microsoft 365 -> Sign-in" ;;
    smtp)       echo "Email: Administration -> Email" ;;
    sharepoint) echo "SharePoint storage: Administration -> Microsoft 365 -> Connection, then -> SharePoint" ;;
  esac
}
remaining=""
for opt in ${CONFIGURE//,/ }; do
  if [[ -n "$(moved_to "$opt")" ]]; then
    printf '   --configure %s is no longer part of the installer. %s.\n' "$opt" "$(moved_to "$opt")"
  else
    remaining+="${remaining:+,}$opt"
  fi
done
if [[ -n "$CONFIGURE" && -z "$remaining" ]]; then
  echo "   Sign in to PDMS as an administrator to change these settings."
  exit 0
fi
CONFIGURE=$remaining

# --- helpers ------------------------------------------------------------------
say()  { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }
note() { printf '   %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*"; }
fail() { printf '\n\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# ask VAR "Prompt" "default" -- PDMS_INSTALL_<VAR> presets the answer.
ask() {
  local var=$1 prompt=$2 default=${3:-} preset_name="PDMS_INSTALL_$1" answer
  local preset=${!preset_name:-}
  if [[ -n "$preset" ]]; then printf -v "$var" '%s' "$preset"; return; fi
  if $NON_INTERACTIVE; then printf -v "$var" '%s' "$default"; return; fi
  if [[ -n "$default" ]]; then
    read -r -p "   $prompt [$default]: " answer || answer=""
  else
    read -r -p "   $prompt: " answer || answer=""
  fi
  printf -v "$var" '%s' "${answer:-$default}"
}

# ask_secret VAR "Prompt" -- input hidden.
ask_secret() {
  local var=$1 prompt=$2 preset_name="PDMS_INSTALL_$1" answer
  local preset=${!preset_name:-}
  if [[ -n "$preset" ]] || $NON_INTERACTIVE; then printf -v "$var" '%s' "$preset"; return; fi
  read -r -s -p "   $prompt (input hidden): " answer || answer=""; echo
  printf -v "$var" '%s' "$answer"
}

# yes_no VAR "Question" default(y|n)
yes_no() {
  local var=$1 prompt=$2 default=$3 preset_name="PDMS_INSTALL_$1" answer
  local preset=${!preset_name:-}
  if [[ -n "$preset" ]]; then answer=$preset
  elif $NON_INTERACTIVE; then answer=$default
  else
    local hint="[y/N]"; [[ $default == y ]] && hint="[Y/n]"
    read -r -p "   $prompt $hint: " answer || answer=""; answer=${answer:-$default}
  fi
  if [[ "$answer" =~ ^([Yy]|[Yy]es|true|1)$ ]]; then printf -v "$var" yes; else printf -v "$var" no; fi
}

port_in_use() {
  (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null && return 0
  if command -v ss >/dev/null; then
    [[ -n "$(ss -Hltn "sport = :$1" 2>/dev/null)" ]] && return 0
  fi
  return 1
}

free_port_from() { local p=$1; while port_in_use "$p"; do p=$((p + 1)); done; echo "$p"; }

valid_port() { [[ "$1" =~ ^[0-9]+$ ]] && (( $1 >= 1 && $1 <= 65535 )); }

rand_hex() { openssl rand -hex "$1"; }

toml() { # a TOML basic string
  local s=${1//\\/\\\\}; s=${s//\"/\\\"}; printf '"%s"' "$s"
}

env_get() { [[ -f .env ]] && sed -n "s/^$1=//p" .env | tail -1 || true; }

env_set() { # env_set KEY VALUE (replaces or appends)
  touch .env; chmod 600 .env
  if grep -q "^$1=" .env; then
    local tmp; tmp=$(mktemp)
    awk -v k="$1" -v v="$2" 'BEGIN{FS=OFS="="} $1==k{print k "=" v; next} {print}' .env > "$tmp" && cat "$tmp" > .env && rm -f "$tmp"
  else
    printf '%s=%s\n' "$1" "$2" >> .env
  fi
}

CONFIG=config/pdms.toml
has_block() { [[ -f $CONFIG ]] && grep -q "^# >>> install.sh:$1\b" "$CONFIG"; }
has_table() { [[ -f $CONFIG ]] && grep -q "^\[$1\]" "$CONFIG"; }

# remove_table "auth.oidc" -- delete an unmanaged table (header to next header).
remove_table() {
  local tmp; tmp=$(mktemp)
  awk -v h="[$1]" '
    $0 == h { skip = 1; next }
    skip && /^\[/ { skip = 0 }
    !skip { print }' "$CONFIG" > "$tmp" && cat "$tmp" > "$CONFIG" && rm -f "$tmp"
}

# write_block NAME [OPTIONS] (content on stdin) -- replace the managed block or
# append it. OPTIONS names the --configure option(s) that rewrite it (default NAME).
write_block() {
  local name=$1 options=${2:-$1} body tmp
  body=$(cat)
  tmp=$(mktemp)
  {
    printf '# >>> install.sh:%s (./install.sh --configure %s rewrites this block)\n' "$name" "$options"
    printf '%s\n' "$body"
    printf '# <<< install.sh:%s\n' "$name"
  } > "$tmp.block"
  if has_block "$name"; then
    awk -v start="# >>> install.sh:$name" -v end="# <<< install.sh:$name" -v f="$tmp.block" '
      index($0, start) == 1 { while ((getline line < f) > 0) print line; skip = 1; next }
      skip && index($0, end) == 1 { skip = 0; next }
      !skip { print }' "$CONFIG" > "$tmp" && cat "$tmp" > "$CONFIG"
  else
    { printf '\n'; cat "$tmp.block"; } >> "$CONFIG"
  fi
  rm -f "$tmp" "$tmp.block"
}

# toml_value TABLE KEY -- a plain string or bare value from config/pdms.toml.
toml_value() {
  [[ -f $CONFIG ]] || return 0
  awk -v h="[$1]" -v k="$2" '
    $0 == h { f = 1; next }
    /^\[/ { f = 0 }
    f && $1 == k { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/^"|"$/, ""); print; exit }' "$CONFIG"
}

# remove_block NAME -- delete a managed block.
remove_block() {
  local tmp; tmp=$(mktemp)
  awk -v start="# >>> install.sh:$1" -v end="# <<< install.sh:$1" '
    index($0, start) == 1 { skip = 1; next }
    skip && index($0, end) == 1 { skip = 0; next }
    !skip { print }' "$CONFIG" > "$tmp" && cat "$tmp" > "$CONFIG" && rm -f "$tmp"
}

# Entra ID sends people back to exactly this address after they sign in. It
# must be listed, character for character (including any port), as a Web
# redirect URI on the app registration, or Entra refuses with AADSTS50011.
redirect_uri() { echo "${PUBLIC_URL_NOW%/}/auth/oidc/callback"; }

detect_timezone() {
  local tz=""
  command -v timedatectl >/dev/null && tz=$(timedatectl show -p Timezone --value 2>/dev/null || true)
  [[ -z "$tz" && -f /etc/timezone ]] && tz=$(cat /etc/timezone)
  [[ -z "$tz" && -L /etc/localtime ]] && tz=$(readlink /etc/localtime | sed 's#.*/zoneinfo/##')
  echo "${tz:-UTC}"
}

# The repository this copy came from, as an https:// URL (for source_url).
git_remote_url() {
  local u
  u=$(git config --get remote.origin.url 2>/dev/null || true)
  u=${u%.git}
  [[ "$u" =~ ^git@([^:]+):(.+)$ ]] && u="https://${BASH_REMATCH[1]}/${BASH_REMATCH[2]}"
  [[ "$u" =~ ^https?:// ]] && echo "$u" || true
}

# set_org_key KEY TOML_VALUE -- set a key inside [org], adding it if missing.
set_org_key() {
  local tmp; tmp=$(mktemp)
  awk -v k="$1" -v v="$2" '
    /^\[org\]/ { in_org = 1; print; next }
    in_org && /^[[:space:]]*$/ { blanks = blanks $0 "\n"; next }
    in_org && /^\[/ { if (!done) { print k " = " v; done = 1 } in_org = 0 }
    in_org && $1 == k { printf "%s", blanks; blanks = ""; if (!done) print k " = " v; done = 1; next }
    { printf "%s", blanks; blanks = ""; print }
    END { if (in_org && !done) print k " = " v; printf "%s", blanks }' "$CONFIG" > "$tmp" && cat "$tmp" > "$CONFIG" && rm -f "$tmp"
}

host_ip() { # the address used to reach the outside world (not a Docker bridge)
  local ip=""
  command -v ip >/dev/null && ip=$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for (i = 1; i < NF; i++) if ($i == "src") { print $(i + 1); exit }}')
  [[ -z "$ip" ]] && ip=$(hostname -I 2>/dev/null | tr ' ' '\n' | grep -Ev '^(172\.(1[6-9]|2[0-9]|3[01])\.|127\.)' | head -1)
  echo "$ip"
}

# A fingerprint of everything the running services read at startup.
settings_hash() { { cat "$CONFIG" config/Caddyfile secrets/* .env 2>/dev/null || true; } | openssl dgst -sha256 | awk '{print $NF}'; }

# --- prerequisites --------------------------------------------------------------
say "PDMS installer"
command -v docker >/dev/null || fail "Docker is required: https://docs.docker.com/engine/install/"
docker compose version >/dev/null 2>&1 || fail "The Docker Compose plugin is required (the 'docker compose' command)."
command -v openssl >/dev/null || fail "openssl is required to generate secrets."
docker info >/dev/null 2>&1 || fail "Cannot talk to Docker. Is it running, and are you in the 'docker' group (or using sudo)?"
mkdir -p config secrets
chmod 700 secrets
SETTINGS_BEFORE=$(settings_hash)

# --- 1. network: how users reach the system ------------------------------------------
# An install made by an earlier installer: adopt its settings rather than asking.
if [[ -z "$(env_get PDMS_PROXY_MODE)" ]] && has_table server && ! configuring network; then
  old_url=$(awk '/^\[server\]/{f=1;next} /^\[/{f=0} f && /^public_url/' "$CONFIG" | sed 's/^public_url = "\(.*\)"$/\1/')
  old_bind=$(env_get PDMS_HTTP_BIND); old_bind=${old_bind:-${PDMS_HTTP_BIND:-127.0.0.1:8080}}
  env_set PDMS_PROXY_MODE custom
  env_set PDMS_PUBLIC_URL "$old_url"
  env_set PDMS_HTTP_BIND "$old_bind"
  env_set PDMS_HTTP_PORT "${old_bind##*:}"
  env_set PDMS_ADMIN_CREATED yes # earlier installers always created one
  echo "   Keeping the existing [server] settings ($old_url). Change them with --configure network."
fi
if configuring network || [[ -z "$(env_get PDMS_PROXY_MODE)" ]]; then
  say "How will people reach PDMS?"
  note "1) Bundled Caddy reverse proxy with automatic HTTPS (Let's Encrypt for a domain)"
  note "2) My own reverse proxy (NGINX Proxy Manager, Traefik, IIS, F5, ...)"
  note "3) Local testing only, plain HTTP on this machine"
  if [[ -n "${PDMS_INSTALL_PROXY:-}" ]]; then
    case "$PDMS_INSTALL_PROXY" in caddy) choice=1 ;; external) choice=2 ;; *) choice=3 ;; esac
  elif $NON_INTERACTIVE; then choice=3
  else read -r -p "   Choice [1/2/3]: " choice || choice=3
  fi

  previous_port=$(env_get PDMS_HTTP_PORT)
  default_port=${previous_port:-$(free_port_from 8080)}
  [[ -z "$previous_port" && "$default_port" != 8080 ]] && note "Port 8080 is already in use on this machine; suggesting $default_port."
  while true; do
    ask PORT "Host port for the PDMS app" "$default_port"
    valid_port "$PORT" || { warn "not a port number: $PORT"; $NON_INTERACTIVE && exit 1; continue; }
    if [[ "$PORT" != "$previous_port" ]] && port_in_use "$PORT"; then
      warn "port $PORT is already in use"; $NON_INTERACTIVE && exit 1; unset PDMS_INSTALL_PORT; continue
    fi
    break
  done

  trusted='"127.0.0.1/32", "172.16.0.0/12"'
  case "$choice" in
    1)
      mode=caddy
      # Caddy normally takes ports 80 and 443. On a machine where something
      # else already has them (another Caddy, a web server), offer the next
      # free ones instead. Ports PDMS's own running Caddy holds count as free.
      caddy_running=false
      docker compose ps --services --status running 2>/dev/null | grep -qx caddy && caddy_running=true
      CADDY_HTTP_PORT="" CADDY_HTTPS_PORT=""
      for kind in HTTP HTTPS; do
        standard=80; [[ $kind == HTTPS ]] && standard=443
        previous=$(env_get "PDMS_CADDY_${kind}_PORT"); previous=${previous:-$standard}
        if $caddy_running; then suggested=$previous; else suggested=$(free_port_from "$previous"); fi
        [[ "$suggested" == "$CADDY_HTTP_PORT" ]] && suggested=$(free_port_from $((suggested + 1)))
        [[ "$suggested" != "$previous" ]] && note "Port $previous is already in use on this machine; suggesting $suggested for Caddy's $kind."
        while true; do
          ask "CADDY_${kind}_PORT" "Host port for Caddy's $kind" "$suggested"
          chosen_name="CADDY_${kind}_PORT"; chosen=${!chosen_name}
          if ! valid_port "$chosen"; then
            warn "not a port number: $chosen"
          elif [[ "$chosen" == "$PORT" || ( $kind == HTTPS && "$chosen" == "$CADDY_HTTP_PORT" ) ]]; then
            warn "port $chosen is already taken by this installation"
          elif port_in_use "$chosen" && ! { $caddy_running && [[ "$chosen" == "$previous" ]]; }; then
            warn "port $chosen is already in use; choose another or stop whatever uses it"
          else
            break
          fi
          $NON_INTERACTIVE && exit 1
          unset "PDMS_INSTALL_CADDY_${kind}_PORT"
        done
      done
      https_suffix=""; [[ "$CADDY_HTTPS_PORT" != 443 ]] && https_suffix=":$CADDY_HTTPS_PORT"
      ask DOMAIN "Domain name pointing at this server (blank to use its IP address)" "$(env_get PDMS_DOMAIN)"
      if [[ -n "$DOMAIN" ]]; then
        public_url="https://$DOMAIN$https_suffix"
        site="$DOMAIN"
        tls_line=""
        note "Caddy will obtain a certificate for $DOMAIN from Let's Encrypt (ports 80 and 443 must be reachable from the internet)."
        if [[ "$CADDY_HTTP_PORT" != 80 || "$CADDY_HTTPS_PORT" != 443 ]]; then
          warn "Let's Encrypt checks the domain on ports 80 and 443. With ports $CADDY_HTTP_PORT and $CADDY_HTTPS_PORT,"
          note "the certificate is only issued if your firewall or router forwards 80 and 443 to them."
          note "For testing without that, leave the domain blank to use an internal certificate."
        fi
      else
        ip=$(host_ip)
        ask PUBLIC_IP "This server's IP address as users will type it" "${ip:-127.0.0.1}"
        public_url="https://$PUBLIC_IP$https_suffix"
        site="$PUBLIC_IP"
        tls_line="	tls internal"
        note "Caddy will use its own internal certificate: browsers warn until you trust it or use a domain."
      fi
      # Global options: Caddy's ports when they are not 80 and 443, and, for an
      # IP address, which certificate to use (browsers send no server name for
      # an IP, and behind Docker's port forwarding Caddy cannot match it either).
      global_lines=""
      [[ "$CADDY_HTTP_PORT" != 80 ]] && global_lines+=$(printf '\thttp_port %s\n' "$CADDY_HTTP_PORT")$'\n'
      [[ "$CADDY_HTTPS_PORT" != 443 ]] && global_lines+=$(printf '\thttps_port %s\n' "$CADDY_HTTPS_PORT")$'\n'
      [[ -z "$DOMAIN" ]] && global_lines+=$(printf '\tdefault_sni %s\n' "$PUBLIC_IP")$'\n'
      globals=""
      [[ -n "$global_lines" ]] && globals=$(printf '{\n%s}\n' "$global_lines")
      cat > config/Caddyfile <<EOF
# Generated by install.sh (./install.sh --configure network rewrites it).
$globals
$site {
$tls_line
	encode zstd gzip
	@metrics path /metrics
	respond @metrics 404
	request_body {
		max_size $UPLOAD_LIMIT
	}
	reverse_proxy app:8080
}
EOF
      bind="127.0.0.1:$PORT"
      env_set PDMS_DOMAIN "$DOMAIN"
      env_set PDMS_CADDY_HTTP_PORT "$CADDY_HTTP_PORT"
      env_set PDMS_CADDY_HTTPS_PORT "$CADDY_HTTPS_PORT"
      ;;
    2)
      mode=external
      ask PUBLIC_URL "The HTTPS address users will open (as served by your proxy)" "$(env_get PDMS_PUBLIC_URL)"
      [[ "$PUBLIC_URL" =~ ^https?:// ]] || fail "the address must start with https://"
      public_url=${PUBLIC_URL%/}
      ask PROXY_IP "Your reverse proxy's IP address (blank if it runs on this machine)" ""
      if [[ -n "$PROXY_IP" ]]; then
        trusted="\"$PROXY_IP/32\", $trusted"
        bind="0.0.0.0:$PORT"
        note "PDMS will listen on all interfaces at port $PORT. Allow only $PROXY_IP to reach it (host firewall)."
      else
        bind="127.0.0.1:$PORT"
      fi
      note "Point your proxy's upstream at http://<this-host>:$PORT, terminate HTTPS there,"
      note "and pass X-Forwarded-For. Allow request bodies up to $UPLOAD_LIMIT."
      ;;
    *)
      mode=local
      public_url="http://localhost:$PORT"
      bind="127.0.0.1:$PORT"
      warn "local testing mode: plain HTTP, insecure cookies, and no Entra ID sign-in (it needs HTTPS)."
      ;;
  esac
  secure=true; [[ "$public_url" == http://* ]] && secure=false
  env_set PDMS_PROXY_MODE "$mode"
  env_set PDMS_HTTP_PORT "$PORT"
  env_set PDMS_HTTP_BIND "$bind"
  env_set PDMS_PUBLIC_URL "$public_url"

  server_block=$(cat <<EOF
[server]
public_url = $(toml "$public_url")
secure_cookies = $secure
# Reverse proxies whose X-Forwarded-For is believed (the Docker network and,
# with your own proxy, its address).
trusted_proxies = [$trusted]
EOF
)
  NETWORK_CHANGED=true
else
  NETWORK_CHANGED=false
fi
MODE=$(env_get PDMS_PROXY_MODE)
PUBLIC_URL_NOW=$(env_get PDMS_PUBLIC_URL)

# --- 2. optional database port for administration tools ------------------------------
if configuring database-port || [[ -z "$(env_get PDMS_DB_PORT)$(env_get PDMS_DB_PORT_DECIDED)" ]]; then
  say "Database access for administration tools (optional)"
  note "PostgreSQL is not reachable from outside Docker by default, and does not need to be."
  note "Expose it on 127.0.0.1 only if you want host tools (psql, pgAdmin over SSH) to reach it."
  if [[ -n "${PDMS_INSTALL_DB_PORT:-}" ]]; then expose=yes
  else yes_no expose "Expose PostgreSQL on 127.0.0.1?" n
  fi
  if [[ $expose == yes ]]; then
    default_db=$(free_port_from 5432)
    [[ "$default_db" != 5432 ]] && note "Port 5432 is already in use; suggesting $default_db."
    [[ "${PDMS_INSTALL_DB_PORT:-}" =~ ^([Yy]es|true)$ ]] && unset PDMS_INSTALL_DB_PORT # "yes": pick a free port
    ask DB_PORT "Host port for PostgreSQL (127.0.0.1 only)" "$default_db"
    valid_port "$DB_PORT" || fail "not a port number: $DB_PORT"
    port_in_use "$DB_PORT" && ! docker compose ps --services --status running 2>/dev/null | grep -qx postgres && fail "port $DB_PORT is already in use"
    cat > docker-compose.override.yml <<EOF
# Generated by install.sh: PostgreSQL on 127.0.0.1:$DB_PORT for host tools.
services:
  postgres:
    ports:
      - "127.0.0.1:$DB_PORT:5432"
EOF
    env_set PDMS_DB_PORT "$DB_PORT"
  else
    rm -f docker-compose.override.yml
    env_set PDMS_DB_PORT ""
  fi
  env_set PDMS_DB_PORT_DECIDED yes
fi

# --- 3. core configuration (first install) -------------------------------------------
if [[ ! -f $CONFIG ]]; then
  say "Name and time zone"
  note "The name appears in the page title, emails, and calendar feeds (e.g. Example Org PDMS)."
  ask NAME "System name" "$DEFAULT_NAME"
  ORG_NAME=$(printf '%s' "$NAME" | tr -d '\r\n' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
  [[ -z "$ORG_NAME" ]] && ORG_NAME="$DEFAULT_NAME"
  (( ${#ORG_NAME} <= 200 )) || fail "the name must be 200 characters or fewer"
  ask TIMEZONE "Time zone (IANA name)" "$(detect_timezone)"
  [[ -e "/usr/share/zoneinfo/$TIMEZONE" || ! -d /usr/share/zoneinfo ]] || warn "\"$TIMEZONE\" is not a known time zone on this host; check it."
  cat > "$CONFIG" <<EOF
# PDMS configuration. Generated by install.sh; edit freely.
# Blocks between "# >>> install.sh:<name>" markers are managed by
# ./install.sh --configure <name>. Secrets are in secrets/, not here.
# Every setting: config/pdms.example.toml and docs/ADMIN_GUIDE.md.

[org]
name = $(toml "$ORG_NAME")
timezone = $(toml "$TIMEZONE")

[log]
format = "json"
filter = "info"

[auth.local]
# Break-glass local administrator sign-in (password + authenticator code).
enabled = true

[notifications]
send_hour = 7
EOF
  echo "   Wrote $CONFIG."
fi
ORG_NAME=$(sed -n 's/^name = "\(.*\)"$/\1/p' "$CONFIG" | head -1)

# --- source code location (AGPL-3.0 section 13) ---------------------------------------
if configuring source || ! grep -q '^source_url' "$CONFIG"; then
  say "Source code location"
  note "PDMS is free software under the AGPL-3.0-or-later: everyone who uses it over the"
  note "network must be able to get its source code, including your changes. Every page links"
  note "to it. Enter your repository's URL (blank to add it later with --configure source)."
  ask SOURCE_URL "Source code URL" "$(git_remote_url)"
  [[ -z "$SOURCE_URL" || "$SOURCE_URL" =~ ^https?:// ]] || fail "the source URL must start with https://"
  set_org_key source_url "$(toml "$SOURCE_URL")"
fi

if $NETWORK_CHANGED; then
  has_table server && ! has_block network && remove_table server
  printf '%s\n' "$server_block" | write_block network
elif ! has_block network && ! has_table server; then
  # Configuration was deleted but .env survived: rebuild [server] from .env.
  printf '[server]\npublic_url = %s\nsecure_cookies = %s\ntrusted_proxies = ["127.0.0.1/32", "172.16.0.0/12"]\n' \
    "$(toml "$PUBLIC_URL_NOW")" "$([[ "$PUBLIC_URL_NOW" == https://* ]] && echo true || echo false)" | write_block network
fi

# --- 4. secrets ----------------------------------------------------------------------
say "Secrets"
new_secret() { # new_secret NAME VALUE -- never overwrites
  if [[ ! -s "secrets/$1" ]]; then printf '%s' "$2" > "secrets/$1"; echo "   generated secrets/$1"; fi
  chmod 644 "secrets/$1"
}
new_secret pg_superuser_password "$(rand_hex 32)"
new_secret pg_owner_password "$(rand_hex 32)"
new_secret pg_app_password "$(rand_hex 32)"
new_secret pdms_secret_key "$(openssl rand -base64 64 | tr -d '\n')"
new_secret db_owner_url "postgres://pdms_owner:$(cat secrets/pg_owner_password)@postgres:5432/pdms"
new_secret db_app_url "postgres://pdms_app:$(cat secrets/pg_app_password)@postgres:5432/pdms"
for f in oidc_client_secret smtp_password graph_certificate; do
  [[ -e "secrets/$f" ]] || : > "secrets/$f"; chmod 644 "secrets/$f"
done
note "Database passwords (256-bit) and the master key (512-bit) are random and stay in secrets/."
note "The directory is private to you; files are readable so containers can mount them."

# --- 7. Removed: Outlook calendar sync --------------------------------------------------
# The Outlook calendar sync was removed (D-087). Tidy what an earlier install wrote:
# the [graph_calendar] block or table, and its client secret file.
if has_block outlook || has_table graph_calendar; then
  awk -v start="# >>> install.sh:outlook" -v end="# <<< install.sh:outlook" '
    index($0, start) == 1 { skip = 1; next }
    skip && index($0, end) == 1 { skip = 0; next }
    !skip { print }' "$CONFIG" > "$CONFIG.tmp" && cat "$CONFIG.tmp" > "$CONFIG" && rm -f "$CONFIG.tmp"
  has_table graph_calendar && remove_table graph_calendar
  note "Removed the Outlook calendar settings: the Outlook sync is no longer part of PDMS."
  note "Events it created stay in that mailbox's calendar until someone deletes them."
fi
rm -f secrets/graph_client_secret

# --- 8. optional containers: OCR and virus scanning ----------------------------------
# The app can't start containers, so the installer decides which run (Compose
# profiles); Administration turns their use on and off. An earlier install
# that set them up keeps them.
[[ -z "$(env_get PDMS_INCLUDE_OCR)" ]] && has_block ocr && \
  env_set PDMS_INCLUDE_OCR "$([[ "$(toml_value ocr sidecar_url)" == http* ]] && echo yes || echo no)"
if [[ -z "$(env_get PDMS_INCLUDE_CLAMAV)" ]] && { has_block storage || has_block clamav || [[ -n "$(env_get PDMS_CLAMAV_DECIDED)" ]]; }; then
  env_set PDMS_INCLUDE_CLAMAV "$(grep -q '^clamav_addr = "clamav' "$CONFIG" && echo yes || echo no)"
fi
if configuring ocr || [[ -z "$(env_get PDMS_INCLUDE_OCR)" ]]; then
  say "OCR service (optional)"
  note "Text in ordinary PDFs and Word files is always read. The OCR service also reads scanned"
  note "PDFs and photos so they are searchable and their dates can be suggested. It is an extra"
  note "container (OCRmyPDF/Tesseract, about 1 GB) and is CPU-heavy while working. Once PDMS runs,"
  note "turn it on under Administration -> Text extraction (OCR)."
  yes_no OCR "Include the OCR service?" n
  env_set PDMS_INCLUDE_OCR "$OCR"
fi
if configuring clamav || [[ -z "$(env_get PDMS_INCLUDE_CLAMAV)" ]]; then
  say "Virus scanner (optional)"
  note "Uploads can be scanned by ClamAV in an extra container (about 1 GB of memory). Once PDMS"
  note "runs, turn scanning on under Administration -> Virus scanning."
  yes_no CLAMAV "Include the virus scanner?" n
  env_set PDMS_INCLUDE_CLAMAV "$CLAMAV"
fi

# --- 8b. upgrade: move settings an earlier installer wrote into Administration -------
# Settings in config/pdms.toml win over Administration and show read-only there
# (D-111), so the blocks earlier installers wrote would lock the admin pages.
MOVED_BLOCKS=(sso smtp sharepoint ocr storage clamav)
blocks_to_move=()
for b in "${MOVED_BLOCKS[@]}"; do has_block "$b" && blocks_to_move+=("$b"); done
MOVE_SETTINGS=no
if (( ${#blocks_to_move[@]} )) && { configuring move-settings || [[ -z "$(env_get PDMS_SETTINGS_MOVED)" ]]; }; then
  say "Settings now managed in Administration"
  note "This install has sign-in, email, SharePoint, OCR or virus-scanning settings in"
  note "$CONFIG (blocks: ${blocks_to_move[*]}). PDMS now sets these up on its admin pages,"
  note "where they can be tested and changed without a restart. Moving them copies them into"
  note "the database (passwords and keys encrypted with the master key), removes those blocks"
  note "from $CONFIG (a copy is kept as $CONFIG.before-move) and empties their secret files."
  note "Settings you wrote by hand outside those blocks stay in the file."
  yes_no MOVE_SETTINGS "Move these settings into Administration?" y
  if [[ $MOVE_SETTINGS == no ]]; then
    env_set PDMS_SETTINGS_MOVED declined
    note "Kept in $CONFIG: they show read-only on the admin pages. Move them later with"
    note "./install.sh --configure move-settings."
  fi
fi

# --- 9. first administrator (asked now; created after the build) -----------------------
ADMIN_EMAIL=""
ADMIN_USERNAME=""
ADMIN_PASSWORD_SHOWN=""
ADMIN_CREATED_NOW=false
if configuring admin || [[ -z "$(env_get PDMS_ADMIN_CREATED)" ]]; then
  say "First administrator (break-glass local account)"
  note "This account signs in with a username (or email), a password and an authenticator app code. Keep it for"
  note "emergencies once Entra ID sign-in works. --configure admin resets it."
  admin_domain=$(env_get PDMS_DOMAIN)
  ask ADMIN_EMAIL "Administrator email" "admin@${admin_domain:-example.org}"
  ask ADMIN_USERNAME "Administrator username" "sysadmin"
  while [[ ! "$ADMIN_USERNAME" =~ ^[A-Za-z0-9._-]{2,64}$ ]]; do
    if $NON_INTERACTIVE || [[ -n "${PDMS_INSTALL_ADMIN_USERNAME:-}" ]]; then
      fail "Administrator username must be 2-64 letters, numbers, dots, dashes or underscores (got \"$ADMIN_USERNAME\")."
    fi
    echo "   Usernames are 2-64 letters, numbers, dots, dashes or underscores."
    ask ADMIN_USERNAME "Administrator username" "sysadmin"
  done
  ask ADMIN_NAME "Administrator name" "PDMS Administrator"
  if [[ -n "${PDMS_INSTALL_ADMIN_PASSWORD:-}" ]]; then
    admin_password=$PDMS_INSTALL_ADMIN_PASSWORD
  else
    admin_password=$(openssl rand -base64 24 | tr -d '/+=' | head -c 24)
    ADMIN_PASSWORD_SHOWN=$admin_password
  fi
fi

# Docker Compose profiles follow the configuration.
profiles=()
[[ "$MODE" == caddy ]] && profiles+=(caddy)
# Hand-written addresses in the file still bring their container along.
{ [[ "$(env_get PDMS_INCLUDE_OCR)" == yes ]] || grep -q '^sidecar_url = "http' "$CONFIG"; } && profiles+=(ocr)
{ [[ "$(env_get PDMS_INCLUDE_CLAMAV)" == yes ]] || grep -q '^clamav_addr = "clamav' "$CONFIG"; } && profiles+=(clamav)
env_set COMPOSE_PROFILES "$(IFS=,; echo "${profiles[*]:-}")"
COMPOSE_PROFILES=$(env_get COMPOSE_PROFILES)
export COMPOSE_PROFILES

# --- 10. build and start (no more questions from here) ---------------------------------
say "Building and starting (the first build takes several minutes)"
docker compose build </dev/null
docker compose up -d postgres </dev/null
docker compose run --rm -T migrate </dev/null

if [[ $MOVE_SETTINGS == yes ]]; then
  say "Moving settings into Administration"
  # Copies them into the database first; if that fails, set -e stops here and
  # nothing is removed.
  docker compose run --rm -T app settings import </dev/null
  cp "$CONFIG" "$CONFIG.before-move"
  for b in "${blocks_to_move[@]}"; do remove_block "$b"; done
  tmp=$(mktemp); cat -s "$CONFIG" > "$tmp" && cat "$tmp" > "$CONFIG" && rm -f "$tmp" # one blank line between tables
  # The secret files of the moved blocks (now in the database). The Graph
  # certificate file is always imported, so it is emptied too.
  [[ " ${blocks_to_move[*]} " == *" sso "* ]] && : > secrets/oidc_client_secret
  [[ " ${blocks_to_move[*]} " == *" smtp "* ]] && : > secrets/smtp_password
  : > secrets/graph_certificate
  env_set PDMS_SETTINGS_MOVED yes
  note "Moved. Change them under Administration from now on."
fi

if [[ -n "$ADMIN_EMAIL" ]]; then
  admins=$(docker compose exec -T postgres psql -U postgres -d pdms -Atc \
    "SELECT count(*) FROM role_assignments a JOIN roles r ON r.id = a.role_id JOIN users u ON u.id = a.user_id
      WHERE r.key = 'admin' AND a.department_id IS NULL AND u.status = 'active'" </dev/null 2>/dev/null || echo 0)
  if configuring admin || [[ "${admins:-0}" == 0 ]]; then
    PDMS_INSTALL_ADMIN_PASSWORD=$admin_password docker compose run --rm -T -e PDMS_INSTALL_ADMIN_PASSWORD app \
      create-admin --email "$ADMIN_EMAIL" --username "$ADMIN_USERNAME" --name "$ADMIN_NAME" --password-env PDMS_INSTALL_ADMIN_PASSWORD </dev/null
    ADMIN_CREATED_NOW=true
  else
    echo "   An administrator already exists; not creating another (use --configure admin to reset)."
    ADMIN_PASSWORD_SHOWN=""
  fi
  env_set PDMS_ADMIN_CREATED yes
fi

docker compose up -d </dev/null
if [[ "$(settings_hash)" != "$SETTINGS_BEFORE" ]]; then
  # Services read configuration and secrets at startup: apply the changes.
  running=$(docker compose ps --services --status running)
  restart=()
  for svc in app worker caddy; do grep -qx "$svc" <<<"$running" && restart+=("$svc"); done
  if [[ ${#restart[@]} -gt 0 ]]; then
    echo "   Applying the new settings (restarting ${restart[*]})..."
    docker compose restart "${restart[@]}" >/dev/null
  fi
fi
for _ in $(seq 1 30); do
  docker compose exec -T app pdms healthcheck >/dev/null 2>&1 && break
  sleep 2
done

say "\"${ORG_NAME}\" is running"
case "$MODE" in
  caddy)    echo "   Open: $PUBLIC_URL_NOW   (Caddy on ports $(env_get PDMS_CADDY_HTTP_PORT | grep . || echo 80)/$(env_get PDMS_CADDY_HTTPS_PORT | grep . || echo 443); the app itself on 127.0.0.1:$(env_get PDMS_HTTP_PORT))" ;;
  external) echo "   Open: $PUBLIC_URL_NOW   (via your reverse proxy -> this host, port $(env_get PDMS_HTTP_PORT))" ;;
  *)        echo "   Open: $PUBLIC_URL_NOW   (local testing only)" ;;
esac
if [[ "$PUBLIC_URL_NOW" == https://* ]]; then
  echo
  echo "   Microsoft sign-in: the Entra app registration must list this redirect URI"
  echo "   (Authentication -> Web), exactly as written, including the port (also shown on"
  echo "   Administration -> Microsoft 365 -> Sign-in):"
  echo "     $(redirect_uri)"
  if $NETWORK_CHANGED; then
    warn "the address changed in this run. If Microsoft sign-in is set up, add the new redirect"
    note "URI above to the app registration (and remove the old one), or Microsoft refuses"
    note "sign-in (AADSTS50011)."
  fi
fi
if $ADMIN_CREATED_NOW; then
  echo
  echo "   Username: $ADMIN_USERNAME"
fi
if [[ -n "$ADMIN_PASSWORD_SHOWN" ]]; then
  echo
  echo "   Administrator password (shown once; store it in your password vault):"
  echo "     $ADMIN_PASSWORD_SHOWN"
  echo "   Add the authenticator secret printed above to an authenticator app."
fi
cat <<EOF

   Sign in and finish setup under Administration: Microsoft sign-in, email, SharePoint,
   the Entra ID user sync, OCR and virus scanning are set up and tested there.
   Settings: .env (ports, profiles), config/pdms.toml (application), secrets/ (keep private).
   Change one part later: ./install.sh --configure network|database-port|source|ocr|clamav|admin
   Back up: deploy/backup/pdms-backup.sh (see docs/BACKUP.md). Check integrity: docker compose run --rm app verify-integrity
   Next steps and testing plan: docs/ROADMAP.md
EOF
