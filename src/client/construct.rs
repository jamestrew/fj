use super::*;

impl Client {
    pub fn new(resolved: ResolvedHost) -> Result<Self> {
        let (base, auth_scheme, platform_url) = match resolved.host.auth_kind {
            AuthKind::Pat => (
                build_base_url(&resolved.name, &resolved.host)?,
                AuthScheme::Pat,
                None,
            ),
            AuthKind::FjordAccount => {
                let fa = resolved.host.fjord_account.as_ref().ok_or_else(|| {
                    anyhow!(
                        "host '{}' is Fjord-Account but missing fjord_account config",
                        resolved.name
                    )
                })?;
                let instance_id = fa.default_instance_id.as_deref().ok_or_else(|| {
                    anyhow!(
                        "no default Fjord instance selected. \
                         Run `fj instances list` and then `fj instances use <id>`."
                    )
                })?;
                (
                    fjord::gateway_base_url(&fa.platform_url, instance_id)?,
                    AuthScheme::FjordBearer,
                    // Remember where to refresh: the platform origin owns the
                    // OIDC token endpoint and the stored refresh token.
                    Some(fa.platform_url.clone()),
                )
            }
        };
        // A proxy credential only ever applies to a direct PAT host.
        let proxy = resolved
            .proxy
            .filter(|_| auth_scheme == AuthScheme::Pat)
            .map(Arc::new);
        let http = build_http(proxy.is_some())?;
        let redirect_http = match proxy {
            Some(_) => Some(build_http(false)?),
            None => None,
        };
        let server_seed = resolved
            .host
            .flavor
            .as_deref()
            .map(Flavor::parse)
            .filter(|flavor| *flavor != Flavor::Unidentified)
            .map(|flavor| {
                ServerInfo::new(
                    flavor,
                    resolved.host.server_version.as_deref().unwrap_or(""),
                )
            });
        Ok(Self {
            account: resolved
                .host
                .selected_account
                .map(account::Guard::new)
                .map(Arc::new),
            http,
            host: resolved.name,
            base,
            token: Arc::new(RwLock::new(resolved.token)),
            auth_scheme,
            platform_url,
            proxy,
            proxy_mode: Arc::new(AtomicU8::new(ProxyMode::Direct.to_u8())),
            redirect_http,
            server: Arc::new(RwLock::new(None)),
            server_seed,
            #[cfg(test)]
            refresh_hook: None,
        })
    }
}
