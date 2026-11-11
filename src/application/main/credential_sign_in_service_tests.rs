// SPDX-License-Identifier: AGPL-3.0-only

use std::cell::Cell;

struct ChildService(std::process::Child);
impl Drop for ChildService {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

struct LifecycleService {
    data: PathBuf,
    loaded: bool,
    disabled: bool,
    fault: &'static str,
    queries: usize,
    events: Rc<RefCell<Vec<&'static str>>>,
    clock: Rc<Cell<Duration>>,
    retain: bool,
    retained: Option<Arc<AdmittedUserHub>>,
    child: Option<ChildService>,
    use_child: bool,
    fleet: bool,
    spawn_on_bootstrap: bool,
    expected_refresh: Option<String>,
    unload_delay: Duration,
}
impl LifecycleService {
    fn new(data: &Path) -> Self {
        Self {
            data: data.to_owned(),
            loaded: true,
            disabled: false,
            fault: "",
            queries: 0,
            events: Rc::new(RefCell::new(Vec::new())),
            clock: Rc::new(Cell::new(Duration::ZERO)),
            retain: false,
            retained: None,
            child: None,
            use_child: false,
            fleet: false,
            spawn_on_bootstrap: false,
            expected_refresh: None,
            unload_delay: Duration::ZERO,
        }
    }
    fn event(&mut self, name: &'static str) -> std::io::Result<()> {
        self.events.borrow_mut().push(name);
        if self.fault == name {
            return Err(std::io::Error::other("synthetic service failure"));
        }
        if self.fault == "panic_restart" && name == "kickstart" {
            panic!("synthetic restart panic");
        }
        Ok(())
    }
    fn spawn_stub(&mut self) -> std::io::Result<()> {
        if self.use_child && self.child.is_none() {
            let mut command = std::process::Command::new(std::env::current_exe()?);
            if self.data.join("stub-config.toml").exists() {
                command.env("V2_STUB_CONFIG", self.data.join("stub-config.toml"));
            }
            let child = command
                .args([
                    "--exact",
                    "g24_m2_cli::ct100_k_release_before_restart",
                    "--test-threads=1",
                ])
                .env("V2_STUB_DATA", &self.data)
                .env("V2_STUB_RESULT", self.data.join("stub-result.json"))
                .env("V2_STUB_FLEET", if self.fleet { "1" } else { "0" })
                .env(
                    "V2_STUB_EXPECTED",
                    self.expected_refresh.as_deref().unwrap_or(""),
                )
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            self.child = Some(ChildService(child));
        }
        Ok(())
    }
    fn operations(&self) -> Vec<&'static str> {
        self.events.borrow().clone()
    }
}
impl ServiceEffects for LifecycleService {
    type Admission = Arc<AdmittedUserHub>;
    type Error = std::io::Error;
    fn now(&self) -> Instant {
        Instant::now() + self.clock.get()
    }
    fn loaded(&mut self, _: Instant) -> std::io::Result<bool> {
        self.queries += 1;
        self.event(if self.queries == 1 {
            "initial_query"
        } else {
            "restart_query"
        })?;
        Ok(self.loaded)
    }
    fn bootout(&mut self, _: Instant) -> std::io::Result<()> {
        if self.fault == "persistent_pause" {
            self.events.borrow_mut().push("disable");
            self.disabled = true;
        }
        self.event("bootout")?;
        self.loaded = false;
        Ok(())
    }
    fn wait_unloaded(&mut self, deadline: Instant) -> std::io::Result<()> {
        self.event("unload")?;
        self.clock.set(self.clock.get() + self.unload_delay);
        if self.now() > deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "synthetic unload deadline",
            ));
        }
        if self.loaded {
            return Err(std::io::Error::other("still loaded"));
        }
        Ok(())
    }
    fn admit(&mut self, _: Instant) -> std::io::Result<Self::Admission> {
        self.event("admission")?;
        let admission = AdmittedUserHub::admit(&self.data).map_err(std::io::Error::other)?;
        if self.retain {
            self.retained = Some(Arc::clone(&admission));
        }
        Ok(admission)
    }
    fn bootstrap(&mut self, _: Instant) -> std::io::Result<()> {
        self.event("bootstrap")?;
        if self.loaded || self.disabled {
            return Err(std::io::Error::other("model bootstrap refused"));
        }
        self.loaded = true;
        if self.spawn_on_bootstrap {
            self.spawn_stub()?;
        }
        Ok(())
    }
    fn kickstart(&mut self, _: Instant) -> std::io::Result<()> {
        self.event("kickstart")?;
        if !self.loaded {
            return Err(std::io::Error::other("model unloaded"));
        }
        self.spawn_stub()
    }
    fn wait_ready(&mut self, deadline: Instant, bounds: ReadinessBounds) -> std::io::Result<()> {
        self.event("readiness")?;
        assert!(
            AdmittedUserHub::admit(&self.data).is_ok() || self.child.is_some(),
            "all command admission holders end before restart"
        );
        if self.child.is_none() {
            return if self.loaded && !self.disabled {
                Ok(())
            } else {
                Err(std::io::Error::other("model not ready"))
            };
        }
        let data = self.data.clone();
        teslatlas_hub::macos_launch_agent::readiness_loop_for_test(
            deadline - self.clock.get(),
            bounds,
            || stub_observation(&data, bounds),
        )
    }
    fn record_unwind_restart_failure(&mut self, _: ServiceFailure<Self::Error>) {
        self.events.borrow_mut().push("unwind_restart_failure");
    }
}

fn stub_observation(data: &Path, bounds: ReadinessBounds) -> std::io::Result<Option<u32>> {
    let Ok(bytes) = fs::read(data.join("stub-result.json")) else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(None);
    };
    if value["admitted"] != true {
        return Err(std::io::Error::other("stub first admission refused"));
    }
    let pid = value["pid"].as_u64().expect("stub PID") as u32;
    let port = value["port"].as_u64().expect("stub port") as u16;
    let mut socket = match std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        bounds.connect,
    ) {
        Ok(socket) => socket,
        Err(_) => return Ok(None),
    };
    // As in the production probe, a transport failure during the exchange (the stub
    // closing a request it read too late, under load) is "not ready yet", not an error.
    let exchange = (move || -> std::io::Result<String> {
        socket.set_read_timeout(Some(bounds.request))?;
        socket.set_write_timeout(Some(bounds.request))?;
        socket.write_all(b"GET /readyz HTTP/1.0\r\n\r\n")?;
        let mut response = String::new();
        socket.take(4096).read_to_string(&mut response)?;
        Ok(response)
    })();
    let Ok(response) = exchange else {
        return Ok(None);
    };
    Ok(
        (response.contains(&format!("x-teslatlas-native-process-id: {pid}\r\n"))
            && response.ends_with("{\"status\":\"ready\"}"))
        .then_some(pid),
    )
}

fn stub_child() -> bool {
    let Some(data) = std::env::var_os("V2_STUB_DATA").map(PathBuf::from) else {
        return false;
    };
    let result = PathBuf::from(std::env::var_os("V2_STUB_RESULT").expect("synthetic result path"));
    let expected = std::env::var("V2_STUB_EXPECTED").expect("expected synthetic pair");
    let fleet = std::env::var("V2_STUB_FLEET").as_deref() == Ok("1");
    let admission = teslatlas_hub::platform::r78_service::stub_admit_and_load_slot_for_test(
        &data,
        (!expected.is_empty()).then_some(expected.as_str()),
        fleet,
    );
    let Ok(_admission) = admission else {
        fs::write(result, br#"{"admitted":false}"#).expect("first admission result");
        return true;
    };
    let identity = std::env::var_os("V2_STUB_CONFIG")
        .map(PathBuf::from)
        .map(|path| HubConfig::load_with_digest(&path).expect("synthetic service configuration"));
    let address = identity.as_ref().map_or(
        std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        |(config, _)| config.bind,
    );
    let digest_header = identity
        .as_ref()
        .map(|(_, digest)| {
            format!(
                "{}: {digest}\r\n",
                teslatlas_hub::server::NATIVE_CONFIG_DIGEST_HEADER
            )
        })
        .unwrap_or_default();
    let listener = std::net::TcpListener::bind(address).expect("stub loopback listener");
    listener.set_nonblocking(true).expect("bounded stub accept");
    let pid = std::process::id();
    fs::write(result, serde_json::to_vec(&serde_json::json!({"admitted":true,"pid":pid,"port":listener.local_addr().expect("stub address").port()})).expect("stub result JSON")).expect("first admission result");
    let gate = std::env::var_os("V2_STUB_GATE").map(PathBuf::from);
    let deadline = Instant::now() + ServiceBudgets::SCALED_TEST.total_service + Duration::from_secs(30);
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut socket, _)) => {
                socket
                    .set_read_timeout(Some(Duration::from_millis(50)))
                    .expect("stub read bound");
                socket
                    .set_write_timeout(Some(Duration::from_millis(50)))
                    .expect("stub write bound");
                let mut request = Zeroizing::new(vec![0; 1024]);
                let mut received = 0;
                while received < request.len() {
                    match socket.read(&mut request[received..]) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => received += count,
                    }
                    if request[..received].windows(4).any(|bytes| bytes == b"\r\n\r\n") { break; }
                }
                let requested = request[..received].windows(4).any(|bytes| bytes == b"\r\n\r\n")
                    && (request[..received].starts_with(b"GET /readyz ") || request[..received].starts_with(b"GET /readyz?"));
                let ready = requested && gate.as_ref().is_none_or(|path| path.exists());
                let (status, body) = if !requested {
                    ("404 Not Found", r#"{"status":"unavailable"}"#)
                } else if ready {
                    ("200 OK", r#"{"status":"ready"}"#)
                } else {
                    fs::write(data.join("stub-probe-observed"), b"1").expect("probe acknowledgement");
                    ("503 Service Unavailable", r#"{"status":"waiting"}"#)
                };
                if let Err(error) = write!(socket, "HTTP/1.0 {status}\r\nx-teslatlas-native-process-id: {pid}\r\n{digest_header}Content-Length: {}\r\n\r\n{body}", body.len()) {
                    assert!(matches!(error.kind(), std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock),
                        "unexpected stub transport error");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(error) => panic!("stub listener: {error}"),
        }
    }
    true
}

fn lifecycle_command(
    issuer: &StubIssuer,
    data: &Path,
    service: &mut LifecycleService,
) -> (Result<(), Box<dyn std::error::Error>>, Zeroizing<Vec<u8>>) {
    let events = Rc::clone(&service.events);
    lifecycle_command_for_slot(issuer, data, service, false, Some(events))
}

fn lifecycle_settings(issuer: &StubIssuer, fleet: bool) -> SignInSettings {
    if fleet {
        SignInSettings::fleet(br#"{"clientId":"client-0001","region":"eu","redirectUri":"http://127.0.0.1/callback","clientSecret":" secret-0001 "}"#)
            .expect("Fleet settings")
            .with_stub_endpoints(&format!("{}/authorize", issuer.endpoint), &format!("{}/token", issuer.endpoint))
            .expect("loopback endpoints")
    } else {
        issuer.settings()
    }
}

fn lifecycle_command_for_slot<E>(
    issuer: &StubIssuer,
    data: &Path,
    service: &mut E,
    fleet: bool,
    events: Option<Rc<RefCell<Vec<&'static str>>>>,
) -> (Result<(), Box<dyn std::error::Error>>, Zeroizing<Vec<u8>>)
where
    E: ServiceEffects<Admission = Arc<AdmittedUserHub>, Error = std::io::Error>,
{
    use tokio::io::AsyncWriteExt;
    let mut config = command_config(data);
    if fleet { config.collector.provider = CollectorProvider::Fleet; }
    let (reader, mut input) = tokio::io::duplex(4096);
    if fleet {
        issuer.runtime.block_on(input.write_all(b"{\"clientId\":\"client-0001\",\"region\":\"eu\",\"redirectUri\":\"http://127.0.0.1/callback\",\"clientSecret\":\" secret-0001 \"}\n"))
            .expect("settings before service effects");
    }
    let mut output = AnsweringOutput {
        runtime: &issuer.runtime,
        input,
        root: data,
        bytes: Zeroizing::default(),
        line_start: 0,
    };
    let (_cancel, mut cancellation) = tokio::sync::watch::channel(false);
    let result = run_sign_in_command_with(
        &config,
        fleet,
        false,
        service,
        SignInEntryOpening::Test { excluded: true },
        reader,
        &mut cancellation,
        &mut output,
        issuer.runtime.handle(),
        |settings| {
            assert!(events.as_ref().is_none_or(|events| events.borrow().is_empty()),
                "settings precede the first service effect");
            match settings {
                Some(settings) => settings.with_stub_endpoints(&format!("{}/authorize", issuer.endpoint), &format!("{}/token", issuer.endpoint)),
                None => Ok(issuer.settings()),
            }
        },
    );
    (result, std::mem::take(&mut output.bytes))
}

#[test]
fn ct100_j_service_duty() {
    for fleet in [false, true] {
    for body in [br#"{"access_token":"access-0001","refresh_token":"refresh-0001","expires_in":28800,"token_type":"Bearer"}"#.as_slice(), br#"{"access_token":"access-0001","refresh_token":"refresh-0001","token_type":"Bearer"}"#.as_slice()] {
        let issuer = StubIssuer::new(body); let data = root(); let mut service = LifecycleService::new(data.path());
        let events = Rc::clone(&service.events);
        let (result, output) = lifecycle_command_for_slot(&issuer, data.path(), &mut service, fleet, Some(events));
        assert_eq!(result.is_ok(), body.windows(10).any(|bytes| bytes == b"expires_in"));
        assert!(std::str::from_utf8(&output).expect("output").contains("authorizationUrl"));
        assert_eq!(service.operations(), ["initial_query","bootout","unload","admission","restart_query","bootstrap","kickstart","readiness"]);
        assert!(service.loaded && !service.disabled);
    }
    }
    let data = root();
    let mut service = LifecycleService::new(data.path());
    service.loaded = false;
    let held = AdmittedUserHub::admit(data.path()).expect("another admitted process");
    let result = sign_in::with_sign_in_scope_for_test(
        &mut service,
        ServiceBudgets::SCALED_TEST,
        data.path(),
        true,
        |_| Ok::<(), CredentialCommandError>(()),
    );
    assert!(matches!(result, Completion::Finished(Err(_))));
    assert_eq!(service.operations(), ["initial_query", "admission"]);
    drop(held);
}

#[test]
fn ct100_k_release_before_restart() {
    if stub_child() {
        return;
    }
    for fleet in [false, true] {
    for ending in ["success", "mismatch", "exclusion", "cancelled"] {
        let issuer = StubIssuer::valid();
        let data = root();
        let mut service = LifecycleService::new(data.path());
        service.use_child = true;
        service.fleet = fleet;
        if ending == "success" {
            service.expected_refresh = Some("refresh-0001".into());
        }
        let result = sign_in::with_sign_in_scope_for_test(
            &mut service,
            ServiceBudgets::SCALED_TEST,
            data.path(),
            ending != "exclusion",
            |entry| {
                if ending == "cancelled" {
                    return Err(sign_in::sign_in_input_error(
                        sign_in::SignInInputError::Cancelled,
                        false,
                    ));
                }
                let attempt = SignInAttempt::begin(entry, lifecycle_settings(&issuer, fleet))?;
                let address = attempt.authorization_url();
                let line = if ending == "mismatch" {
                    Zeroizing::new(
                        r#"{"redirectUrl":"http://127.0.0.1/callback?state=wrong&code=code-0001"}"#
                            .into(),
                    )
                } else {
                    answer(&address, "code=code-0001")
                };
                issuer
                    .runtime
                    .block_on(attempt.redeem(line.as_bytes(), |_| Ok(())))
            },
        );
        assert!(
            matches!(result, Completion::Finished(_)),
            "restart must be confirmed: {ending}: {result:?}"
        );
        assert_eq!(
            matches!(result, Completion::Finished(Ok(()))),
            ending == "success"
        );
        let result: serde_json::Value = serde_json::from_slice(
            &fs::read(data.path().join("stub-result.json")).expect("child admission result"),
        )
        .expect("result JSON");
        assert_eq!(result["admitted"], true);
        assert!(
            service
                .child
                .as_mut()
                .expect("owned child")
                .0
                .try_wait()
                .expect("child status")
                .is_none()
        );
    }
    }
    let data = root();
    let mut service = LifecycleService::new(data.path());
    service.retain = true;
    let result = sign_in::with_sign_in_scope_for_test(
        &mut service,
        ServiceBudgets::SCALED_TEST,
        data.path(),
        true,
        |_| Ok::<(), CredentialCommandError>(()),
    );
    assert!(
        matches!(result, Completion::RestartFailed { .. }),
        "M-ADMISSION-CLONE must fail unique release"
    );
    assert!(!service.operations().contains(&"kickstart"));
}

#[test]
fn ct100_l_partial_stops() {
    for fault in ["bootout", "unload", "admission", "initial_query"] {
        let data = root();
        let mut service = LifecycleService::new(data.path());
        service.fault = fault;
        let called = Cell::new(false);
        let result = sign_in::with_sign_in_scope_for_test(
            &mut service,
            ServiceBudgets::SCALED_TEST,
            data.path(),
            true,
            |_| {
                called.set(true);
                Ok::<(), CredentialCommandError>(())
            },
        );
        assert!(
            matches!(result, Completion::Finished(Err(_))),
            "{fault}: {result:?}"
        );
        assert!(!called.get());
        assert_eq!(
            service
                .operations()
                .iter()
                .filter(|op| **op == "kickstart")
                .count(),
            usize::from(fault != "initial_query")
        );
        if fault == "bootout" {
            assert!(!service.operations().contains(&"bootstrap"));
        }
    }
    let data = root();
    let mut service = LifecycleService::new(data.path());
    service.unload_delay = Duration::from_secs(31);
    let result = sign_in::with_sign_in_scope_for_test(
        &mut service,
        ServiceBudgets::SCALED_TEST,
        data.path(),
        true,
        |_| Ok::<(), CredentialCommandError>(()),
    );
    assert!(matches!(result, Completion::Finished(Err(_))));
    assert!(service.operations().contains(&"kickstart"));
    assert!(!service.operations().contains(&"admission"));
}

#[test]
fn ct100_m_input_and_cancel() {
    use tokio::io::AsyncWriteExt;
    for kind in ["partial", "oversize", "eof", "cancel"] {
        let data = root();
        let mut service = LifecycleService::new(data.path());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("input runtime");
        let result = sign_in::with_sign_in_scope_for_test(
            &mut service,
            ServiceBudgets::SCALED_TEST,
            data.path(),
            true,
            |_| {
                let error = runtime.block_on(async {
                    tokio::time::pause();
                    let (mut writer, reader) = tokio::io::duplex(32_768);
                    let (sender, mut cancelled) = tokio::sync::watch::channel(false);
                    match kind {
                        "partial" => writer.write_all(&[b'x'; 100]).await.expect("partial line"),
                        "oversize" => writer
                            .write_all(&vec![b'x'; 16_384])
                            .await
                            .expect("over-cap line"),
                        "eof" => drop(writer),
                        _ => {
                            sender.send(true).expect("cancellation");
                        }
                    }
                    sign_in::SignInInput::new(reader)
                        .line(
                            tokio::time::Instant::now() + Duration::from_secs(600),
                            &mut cancelled,
                        )
                        .await
                        .expect_err("bounded input ends")
                });
                assert_eq!(
                    error,
                    match kind {
                        "partial" => sign_in::SignInInputError::TimedOut,
                        "oversize" => sign_in::SignInInputError::TooLong,
                        "eof" => sign_in::SignInInputError::Closed,
                        _ => sign_in::SignInInputError::Cancelled,
                    }
                );
                Err::<(), _>(sign_in::sign_in_input_error(error, false))
            },
        );
        assert!(matches!(result, Completion::Finished(Err(_))));
        assert!(service.operations().contains(&"readiness"));
        assert!(!projection(data.path()).legacy_credentials.present);
    }
}

#[test]
fn ct100_n_deadlines() {
    let budgets = ServiceBudgets::PRODUCTION;
    let scaled = ServiceBudgets::SCALED_TEST;
    assert_eq!(
        (
            budgets.command.as_secs(),
            budgets.unload.as_secs(),
            budgets.admission.as_secs(),
            budgets.stop.as_secs(),
            budgets.restart.as_secs(),
            budgets.total_service.as_secs()
        ),
        (30, 300, 30, 390, 150, 540)
    );
    assert_eq!(budgets.stop, budgets.unload + budgets.command * 3);
    assert_eq!(budgets.total_service, budgets.stop + budgets.restart);
    assert_eq!(budgets.total_service.as_secs() + 20 + 600 + 60, 1220);
    assert_eq!(scaled.stop * 10, budgets.stop);
    assert_eq!(scaled.restart * 10, budgets.restart);
    assert_eq!(scaled.readiness.stable_observations, 10);
    let began = Instant::now();
    let error =
        teslatlas_hub::platform::r78_service::sleeping_helper_for_test(began + scaled.command)
            .expect_err("hung child must be killed and reaped");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(began.elapsed() <= scaled.command + Duration::from_millis(50));
    let observations = Cell::new(0);
    teslatlas_hub::macos_launch_agent::readiness_loop_for_test(
        Instant::now() + scaled.restart,
        scaled.readiness,
        || {
            let count = observations.get() + 1;
            observations.set(count);
            Ok((count > 51).then_some(910_000_001))
        },
    )
    .expect("readiness is deadline-bound, beyond fifty observations");
    assert_eq!(observations.get(), 61);
    let data = root();
    let mut service = LifecycleService::new(data.path());
    service.unload_delay = Duration::from_secs(10);
    assert!(
        matches!(
            sign_in::with_sign_in_scope_for_test(
                &mut service,
                scaled,
                data.path(),
                true,
                |_| Ok::<(), CredentialCommandError>(())
            ),
            Completion::Finished(Ok(()))
        ),
        "M-SHORT-STOP cannot admit a slow unload"
    );
}

#[test]
fn ct100_o_storage_pause() {
    let data = root();
    let mut service = LifecycleService::new(data.path());
    let clock = Rc::clone(&service.clock);
    let issuer = StubIssuer::valid();
    let result = sign_in::with_sign_in_scope_for_test(
        &mut service,
        ServiceBudgets::SCALED_TEST,
        data.path(),
        true,
        |entry| {
            clock.set(Duration::from_secs(60));
            let attempt = SignInAttempt::begin(entry, issuer.settings())?;
            let address = attempt.authorization_url();
            let line = answer(&address, "code=code-0001");
            issuer
                .runtime
                .block_on(attempt.redeem(line.as_bytes(), |_| Ok(())))
        },
    );
    assert!(
        matches!(result, Completion::Finished(Ok(()))),
        "storage does not spend either service share"
    );
    assert!(projection(data.path()).legacy_credentials.present);
    assert_eq!(service.operations().last(), Some(&"readiness"));
}

#[test]
fn ct100_q_partial_start_command() {
    #[cfg(not(feature = "credential-relocation-fixture"))]
    if command_process_cases::child_role() { return; }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    let process_deadline = Instant::now() + ServiceBudgets::SCALED_TEST.total_service + Duration::from_secs(30);
    let issuer = StubIssuer::valid();
    let data = root();
    let mut service = LifecycleService::new(data.path());
    service.fault = "kickstart";
    service.use_child = true;
    service.spawn_on_bootstrap = true;
    service.expected_refresh = Some("refresh-0001".into());
    let (result, output) = lifecycle_command(&issuer, data.path(), &mut service);
    assert!(
        result
            .expect_err("unconfirmed restart is operational failure")
            .to_string()
            .contains("sign_in_service_restart_failed")
    );
    assert!(
        std::str::from_utf8(&output)
            .expect("output")
            .contains("signed_in")
    );
    assert!(projection(data.path()).legacy_credentials.present);
    teslatlas_hub::macos_launch_agent::readiness_loop_for_test(
        Instant::now() + ServiceBudgets::SCALED_TEST.restart,
        ServiceBudgets::SCALED_TEST.readiness,
        || stub_observation(data.path(), ServiceBudgets::SCALED_TEST.readiness),
    )
    .expect("a partially started child remains admitted and usable");
    assert!(
        service
            .child
            .as_mut()
            .expect("partially started service")
            .0
            .try_wait()
            .expect("child status")
            .is_none()
    );
    for fault in ["kickstart", "panic_restart"] {
        let data = root();
        let mut service = LifecycleService::new(data.path());
        service.fault = fault;
        let result = sign_in::with_sign_in_scope_for_test(
            &mut service,
            ServiceBudgets::SCALED_TEST,
            data.path(),
            true,
            |_| -> Result<(), CredentialCommandError> { panic!("synthetic storage panic") },
        );
        assert!(
            matches!(
                result,
                Completion::RestartFailed {
                    original: Err(_),
                    ..
                }
            ),
            "original panic and restart failure both survive: {result:?}"
        );
    }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    command_process_cases::run_all(process_deadline);
}

#[cfg(not(feature = "credential-relocation-fixture"))]
mod command_process_cases {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use teslatlas_hub::platform::r78_service::{InstalledServiceEffects, service_runner_for_test};

    fn event(data: &Path, name: &str) -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new().create(true).append(true).open(data.join("command-events"))?;
        writeln!(file, "{name}")
    }

    fn events(data: &Path) -> Vec<String> {
        fs::read_to_string(data.join("command-events")).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    fn result(data: &Path) -> Option<serde_json::Value> {
        serde_json::from_slice(&fs::read(data.join("stub-result.json")).ok()?).ok()
    }

    fn helper(data: &Path, phase: &str, deadline: Instant) -> std::io::Result<()> {
        if data.join("q-test-stop").exists() { return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "synthetic wall cap")); }
        if Instant::now() >= deadline { return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "synthetic helper deadline")); }
        event(data, &format!("helper_{phase}_spawned"))?;
        let mut child = ChildService(std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "g24_m2_cli::ct100_q_partial_start_command", "--test-threads=1"])
            .env("V2_Q_ROLE", "helper")
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?);
        loop {
            if let Some(status) = child.0.try_wait()? {
                child.0.wait()?;
                event(data, &format!("helper_{phase}_reaped"))?;
                return if status.success() { Ok(()) } else { Err(std::io::Error::other("synthetic helper refused")) };
            }
            if Instant::now() >= deadline {
                let _ = child.0.kill();
                child.0.wait()?;
                event(data, &format!("helper_{phase}_reaped"))?;
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "synthetic helper deadline"));
            }
            std::thread::sleep(Duration::from_millis(2).min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    fn output(code: i32, bytes: Vec<u8>) -> std::process::Output {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: bytes,
            stderr: if code == 113 { b"Could not find service".to_vec() } else { Vec::new() },
        }
    }

    pub(super) fn child_role() -> bool {
        let Ok(role) = std::env::var("V2_Q_ROLE") else { return false; };
        let data = PathBuf::from(std::env::var_os("V2_Q_ROOT").expect("explicit synthetic root"));
        let run = std::env::var("V2_Q_RUN").expect("synthetic run");
        assert!(data.is_absolute() && run.len() == 32);
        assert!(fs::read(data.join("q-run.marker")).expect("synthetic marker") == run.as_bytes());
        match role.as_str() {
            "helper" => std::process::exit(0),
            "command" => command_child(&data, &run),
            _ => panic!("unrecognised synthetic role"),
        }
    }

    struct PublishedOutput<'a> {
        inner: AnsweringOutput<'a>,
        file: fs::File,
        snapshot_acknowledged: bool,
    }
    impl Write for PublishedOutput<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let count = self.inner.write(bytes)?;
            self.file.write_all(&bytes[..count])?;
            self.file.flush()?;
            if !self.snapshot_acknowledged && self.inner.bytes.ends_with(b"\n")
                && self.inner.bytes.windows(b"\"status\":\"signed_in\"".len()).any(|bytes| bytes == b"\"status\":\"signed_in\"")
            {
                self.snapshot_acknowledged = true;
                let deadline = Instant::now() + ServiceBudgets::SCALED_TEST.total_service + Duration::from_secs(30);
                while !self.inner.root.join("snapshot-observed").exists() {
                    if Instant::now() >= deadline { return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "synthetic snapshot acknowledgement")); }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> { self.file.flush() }
    }

    fn command_child(data: &Path, run: &str) -> ! {
        use tokio::io::AsyncWriteExt;
        let fleet = std::env::var("V2_Q_FLEET").as_deref() == Ok("1");
        let ending = std::env::var("V2_Q_ENDING").expect("synthetic ending");
        let reserve = std::net::TcpListener::bind("127.0.0.1:0").expect("synthetic port");
        let port = reserve.local_addr().expect("address").port();
        let config_file = data.join("stub-config.toml");
        let provider = if fleet { "provider = 'fleet'\n" } else { "" };
        fs::write(&config_file, format!("data_dir = {data:?}\nbind = '127.0.0.1:{port}'\n[collector]\n{provider}interval_seconds = 0\n")).expect("synthetic config");
        let action = if fleet { "setup-fleet" } else { "setup" };
        let cli = Cli::try_parse_from(["hub", "--config", config_file.to_str().expect("config path"), action, "--sign-in"]).expect("real command grammar");
        let fleet = match &cli.command {
            Command::Setup { sign_in: true, .. } => false,
            Command::SetupFleet { sign_in: true, .. } => true,
            _ => panic!("command parser selected another entry path"),
        };
        let config = HubConfig::load(cli.config.as_deref().expect("explicit config")).expect("command config");
        let plist = data.join("fixture.r78.plist");
        fs::write(&plist, b"synthetic plist decoded by runner").expect("synthetic plist");
        let selected = data.to_owned();
        let selected_config = config_file.clone();
        let selected_ending = ending.clone();
        let restart_started = Rc::new(Cell::new(None::<Instant>));
        let selected_restart = Rc::clone(&restart_started);
        let mut loaded = true;
        let hooks = service_runner_for_test(plist, run, Box::new(move |program, args, deadline| {
            if program == Path::new("/usr/bin/plutil") {
                event(&selected, "plist_decode")?;
                helper(&selected, "plist", deadline)?;
                return Ok(output(0, serde_json::to_vec(&serde_json::json!({"ProgramArguments":["/synthetic/hub","--config",selected_config,"serve"]})).map_err(std::io::Error::other)?));
            }
            assert_eq!(program, Path::new("/bin/launchctl"));
            let action = args.first().and_then(|arg| arg.to_str()).expect("service action");
            event(&selected, action)?;
            match action {
                "print" => {
                    if selected.join("snapshot-observed").exists() && selected_restart.get().is_none() {
                        selected_restart.set(Some(Instant::now()));
                    }
                    if selected_ending == "abrupt" && selected.join("stub-probe-observed").exists() {
                        // The parent kills the command at this acknowledged readiness point.
                        // No helper remains alive or is constructed after that acknowledgement.
                        fs::write(selected.join("command-kill-ready"), b"1")?;
                        while Instant::now() < deadline { std::thread::sleep(Duration::from_millis(2)); }
                        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "synthetic observation gate"));
                    }
                    helper(&selected, "print", deadline)?;
                    if !loaded { return Ok(output(113, Vec::new())); }
                    let pid = result(&selected).and_then(|value| value["pid"].as_u64());
                    Ok(output(0, pid.map(|pid| format!("state = running\npid = {pid}\n")).unwrap_or_else(|| "state = waiting\n".into()).into_bytes()))
                }
                "bootout" => {
                    helper(&selected, "bootout", deadline)?;
                    loaded = false;
                    event(&selected, "bootout_ok")?;
                    Ok(output(0, Vec::new()))
                }
                "bootstrap" => {
                    assert!(!loaded, "bootstrap only reloads an absent job");
                    helper(&selected, "bootstrap", deadline)?;
                    loaded = true;
                    event(&selected, "bootstrap_ok")?;
                    Ok(output(0, Vec::new()))
                }
                "kickstart" => {
                    let began = Instant::now();
                    while result(&selected).is_none() {
                        if Instant::now() >= deadline { return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "synthetic service acknowledgement")); }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    if selected_ending == "kickstart_hang" {
                        event(&selected, "helper_kickstart_spawned")?;
                        let result = teslatlas_hub::platform::r78_service::sleeping_helper_for_test(deadline);
                        fs::write(selected.join("kickstart-elapsed-us"), began.elapsed().as_micros().to_string())?;
                        event(&selected, "helper_kickstart_reaped")?;
                        if !result.as_ref().is_err_and(|error| error.kind() == std::io::ErrorKind::TimedOut) {
                            return Err(std::io::Error::other("synthetic hung helper did not time out"));
                        }
                        event(&selected, "kickstart_timeout_confirmed")?;
                        result?;
                    } else {
                        helper(&selected, "kickstart", deadline)?;
                        fs::write(selected.join("kickstart-elapsed-us"), began.elapsed().as_micros().to_string())?;
                    }
                    Ok(output(if selected_ending == "kickstart_error" { 1 } else { 0 }, Vec::new()))
                }
                _ => Err(std::io::Error::other("synthetic action refused")),
            }
        })).expect("isolated service runner");
        let issuer = StubIssuer::valid();
        drop(reserve);
        let request_path = data.join("command-requests");
        issuer.exchange.lock().expect("exchange").observe_request = Some(Arc::new(move || {
            let mut file = fs::OpenOptions::new().create(true).append(true).open(&request_path).expect("request counter");
            file.write_all(b"request\n").expect("request event");
        }));
        let (reader, mut input) = tokio::io::duplex(4096);
        if fleet {
            issuer.runtime.block_on(input.write_all(b"{\"clientId\":\"client-0001\",\"region\":\"eu\",\"redirectUri\":\"http://127.0.0.1/callback\",\"clientSecret\":\" secret-0001 \"}\n")).expect("settings");
        }
        let mut writer = PublishedOutput {
            inner: AnsweringOutput { runtime: &issuer.runtime, input, root: data, bytes: Zeroizing::default(), line_start: 0 },
            file: fs::File::create(data.join("command-stdout")).expect("command output"),
            snapshot_acknowledged: false,
        };
        let (_sender, mut cancelled) = tokio::sync::watch::channel(false);
        let mut backend = InstalledServiceEffects::new(data, ServiceBudgets::SCALED_TEST);
        let result = run_sign_in_command_with(&config, fleet, false, &mut backend, SignInEntryOpening::Test { excluded: true }, reader, &mut cancelled, &mut writer, issuer.runtime.handle(), |settings| match settings {
            Some(settings) => settings.with_stub_endpoints(&format!("{}/authorize", issuer.endpoint), &format!("{}/token", issuer.endpoint)),
            None => Ok(issuer.settings()),
        });
        if let Some(began) = restart_started.get() {
            fs::write(data.join("restart-elapsed-us"), began.elapsed().as_micros().to_string()).expect("restart duration");
        }
        let mut errors = fs::File::create(data.join("command-stderr")).expect("command error output");
        let code = report_outcome(result, &mut errors);
        errors.flush().expect("error output flushed");
        drop(backend);
        drop(hooks);
        std::process::exit(i32::from(code))
    }

    type PairSnapshot = Vec<(String, Vec<u8>, Vec<u8>, String)>;
    fn pairs(data: &Path) -> Option<PairSnapshot> {
        let connection = rusqlite::Connection::open_with_flags(data.join("hub.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        connection.busy_timeout(Duration::ZERO).ok()?;
        let mut query = connection.prepare("SELECT slot, access_envelope, refresh_envelope, generation FROM credential_pairs ORDER BY slot").ok()?;
        let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).ok()?.collect::<Result<Vec<_>, _>>();
        rows.ok()
    }

    fn bindings(data: &Path) -> i64 {
        let connection = rusqlite::Connection::open_with_flags(data.join("hub.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).expect("read-only binding inspection");
        connection.busy_timeout(Duration::ZERO).expect("bounded binding inspection");
        connection.query_row("SELECT count(*) FROM credential_sign_ins", [], |row| row.get(0)).expect("binding count")
    }

    pub(super) fn run_all(deadline: Instant) {
        for fleet in [false, true] {
            for ending in ["readiness_timeout", "kickstart_error", "kickstart_hang", "abrupt"] {
                run_case(fleet, ending, deadline);
            }
        }
    }

    fn run_case(fleet: bool, ending: &str, deadline: Instant) {
        let stop_cutoff = deadline - ServiceBudgets::SCALED_TEST.command * 2;
        let data = root();
        let run = uuid::Uuid::new_v4().simple().to_string();
        fs::write(data.path().join("q-run.marker"), run.as_bytes()).expect("synthetic marker");
        assert!(Instant::now() < stop_cutoff, "CT100(q) reserves time to reap its helpers");
        let mut command = ChildService(std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "g24_m2_cli::ct100_q_partial_start_command", "--test-threads=1"])
            .env("V2_Q_ROLE", "command").env("V2_Q_ROOT", data.path()).env("V2_Q_RUN", &run)
            .env("V2_Q_FLEET", if fleet { "1" } else { "0" }).env("V2_Q_ENDING", ending)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("owned sign-in process"));
        let mut service: Option<ChildService> = None;
        let mut stopped = 0;
        let mut before: Option<PairSnapshot> = None;
        let mut killed = false;
        let mut cap_requested = false;
        let status = loop {
            if Instant::now() >= stop_cutoff && !cap_requested {
                fs::write(data.path().join("q-test-stop"), b"1").expect("request bounded helper shutdown");
                cap_requested = true;
            }
            assert!(Instant::now() < deadline, "CT100(q) command process wall cap");
            let trace = events(data.path());
            let stops = trace.iter().filter(|name| *name == "bootout_ok").count();
            if stops > stopped {
                if stopped != 0 { drop(service.take()); }
                stopped = stops;
            }
            if !cap_requested && service.is_none() && trace.iter().any(|name| name == "bootstrap_ok") && stopped == 1 {
                service = Some(ChildService(std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", "g24_m2_cli::ct100_k_release_before_restart", "--test-threads=1"])
                    .env("V2_STUB_DATA", data.path()).env("V2_STUB_RESULT", data.path().join("stub-result.json"))
                    .env("V2_STUB_EXPECTED", "refresh-0001").env("V2_STUB_FLEET", if fleet { "1" } else { "0" })
                    .env("V2_STUB_CONFIG", data.path().join("stub-config.toml"))
                    .env("V2_STUB_GATE", data.path().join("readiness-gate"))
                    .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("independent service process")));
            }
            let published = Zeroizing::new(fs::read_to_string(data.path().join("command-stdout")).unwrap_or_default());
            if before.is_none() && published.contains("\"status\":\"signed_in\"") {
                before = pairs(data.path()).filter(|rows| rows.len() == 1);
                if before.is_some() {
                    assert_eq!(bindings(data.path()), 0);
                    fs::write(data.path().join("snapshot-observed"), b"1").expect("acknowledge the pair before restart");
                }
            }
            if ending == "abrupt" && !killed && data.path().join("command-kill-ready").exists() {
                assert!(data.path().join("stub-probe-observed").exists());
                assert!(!data.path().join("command-stderr").exists(), "kill occurs before the command returns from readiness");
                assert!(trace.iter().any(|name| name == "helper_kickstart_reaped"));
                assert_eq!(trace.iter().filter(|name| name.ends_with("_spawned")).count(), trace.iter().filter(|name| name.ends_with("_reaped")).count());
                command.0.kill().expect("kill the owned sign-in process only");
                killed = true;
            }
            if let Some(status) = command.0.try_wait().expect("command status") { break status; }
            std::thread::sleep(Duration::from_millis(2));
        };
        command.0.wait().expect("reap command");
        assert!(!cap_requested, "CT100(q) work exceeded its wall budget");
        if ending == "abrupt" { assert!(killed && status.signal() == Some(9)); }
        else { assert!(!status.success(), "unconfirmed restart is an operational failure"); }
        let published = Zeroizing::new(fs::read_to_string(data.path().join("command-stdout")).expect("command output"));
        assert!(published.contains("\"status\":\"signed_in\""));
        assert!(published.contains(if fleet { "\"slot\":\"fleet_api\"" } else { "\"slot\":\"owner_api\"" }));
        let errors = Zeroizing::new(fs::read_to_string(data.path().join("command-stderr")).unwrap_or_default());
        if ending != "abrupt" { assert!(errors.contains("sign_in_service_restart_failed")); }
        else { assert!(errors.is_empty(), "the abruptly killed command publishes no restart report"); }
        let trace = events(data.path());
        assert_eq!(trace.iter().filter(|name| *name == "bootout").count(), 1);
        assert_eq!(trace.iter().filter(|name| *name == "bootout_ok").count(), 1);
        assert_eq!(trace.iter().filter(|name| *name == "bootstrap_ok").count(), 1);
        assert_eq!(trace.iter().filter(|name| *name == "bootstrap").count(), 1);
        assert_eq!(trace.iter().filter(|name| *name == "kickstart").count(), 1);
        assert!(!trace.iter().any(|name| name == "disable" || name == "enable"));
        assert_eq!(trace.iter().filter(|name| name.ends_with("_spawned")).count(), trace.iter().filter(|name| name.ends_with("_reaped")).count());
        if ending != "abrupt" {
            let elapsed = fs::read_to_string(data.path().join("restart-elapsed-us")).expect("restart duration").parse::<u128>().expect("duration count");
            assert!(elapsed <= (ServiceBudgets::SCALED_TEST.restart + Duration::from_millis(50)).as_micros(), "restart share bound");
        }
        if ending == "kickstart_hang" { assert!(trace.iter().any(|name| name == "kickstart_timeout_confirmed")); }
        let elapsed = fs::read_to_string(data.path().join("kickstart-elapsed-us")).expect("helper duration").parse::<u128>().expect("duration count");
        assert!(elapsed <= (ServiceBudgets::SCALED_TEST.command + Duration::from_millis(50)).as_micros(), "helper run limit");
        if ending == "kickstart_error" || ending == "kickstart_hang" {
            assert!(!trace.iter().any(|name| name == "plist_decode"), "failed start does not enter readiness");
        } else {
            assert!(trace.iter().any(|name| name == "plist_decode"));
        }
        let before = before.expect("committed pair observed before restart");
        assert!(pairs(data.path()).expect("retained pair") == before, "outcome preserves the written pair and generation");
        assert_eq!(bindings(data.path()), 0);
        assert_eq!(fs::read_to_string(data.path().join("command-requests")).expect("request count").lines().count(), 1);
        let acknowledgement = result(data.path()).expect("first service admission acknowledgement");
        assert_eq!(acknowledgement["admitted"], true);
        let pid = acknowledgement["pid"].as_u64().expect("service PID") as u32;
        let service = service.as_mut().expect("independent service survives the command");
        assert_eq!(service.0.id(), pid);
        assert!(service.0.try_wait().expect("service status").is_none());
        assert!(matches!(AdmittedUserHub::admit(data.path()), Err(teslatlas_hub::hub_user_process::UserLifetimeLockError::AlreadyRunning)));
        fs::write(data.path().join("readiness-gate"), b"1").expect("release readiness after command exit");
        teslatlas_hub::macos_launch_agent::readiness_loop_for_test(deadline, ServiceBudgets::SCALED_TEST.readiness,
            || stub_observation(data.path(), ServiceBudgets::SCALED_TEST.readiness)).expect("same admitted PID reaches readiness");
        assert!(service.0.try_wait().expect("service remains live").is_none());
        assert!(pairs(data.path()).expect("pair after readiness") == before);
        assert_eq!(bindings(data.path()), 0);
    }
}

#[cfg(not(feature = "credential-relocation-fixture"))]
fn native_lifecycle_case(fault: &'static str, fleet: bool) {
    use std::os::unix::process::ExitStatusExt;
    use teslatlas_hub::platform::r78_service::{InstalledServiceEffects, service_runner_for_test};
    fn output(code: i32, stdout: Vec<u8>, stderr: &[u8]) -> std::process::Output {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout,
            stderr: stderr.to_vec(),
        }
    }
    let data = root();
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").expect("synthetic port");
    let port = reserve.local_addr().expect("synthetic address").port();
    let config = data.path().join("stub-config.toml");
    let provider = if fleet { "provider = 'fleet'\n" } else { "" };
    fs::write(
        &config,
        format!(
            "data_dir = {:?}\nbind = '127.0.0.1:{port}'\n[collector]\n{provider}interval_seconds = 0\n",
            data.path()
        ),
    )
    .expect("synthetic config");
    let plist = data.path().join("fixture.r78.plist");
    fs::write(&plist, b"synthetic plist decoded by runner").expect("synthetic plist");
    let state = Rc::new(RefCell::new(LifecycleService::new(data.path())));
    state.borrow_mut().use_child = true;
    state.borrow_mut().fleet = fleet;
    if fault.is_empty() { state.borrow_mut().expected_refresh = Some("refresh-0001".into()); }
    state.borrow_mut().fault = fault;
    let events = Rc::clone(&state.borrow().events);
    let selected = Rc::clone(&state);
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let hooks = service_runner_for_test(plist, &run_id, Box::new(move |program, arguments, _deadline| {
        if program == Path::new("/usr/bin/plutil") {
            selected.borrow().events.borrow_mut().push("plist_decode");
            return Ok(output(0, serde_json::to_vec(&serde_json::json!({"ProgramArguments":["/synthetic/hub","--config",config,"serve"]})).expect("plist JSON"), b""));
        }
        assert_eq!(program, Path::new("/bin/launchctl"));
        let mut state = selected.borrow_mut();
        match arguments.first().and_then(|value| value.to_str()).expect("service action") {
            "print" => {
                state.events.borrow_mut().push("print"); state.queries += 1;
                if state.fault == "unload" && state.queries == 2 { return Err(std::io::Error::other("synthetic unload query failed")); }
                if !state.loaded { return Ok(output(113, Vec::new(), b"Could not find service")); }
                let pid = fs::read(state.data.join("stub-result.json")).ok().and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok()).and_then(|value| value["pid"].as_u64());
                Ok(output(0, pid.map(|pid| format!("state = running\npid = {pid}\n")).unwrap_or_else(|| "state = waiting\n".into()).into_bytes(), b""))
            }
            "disable" => { state.events.borrow_mut().push("disable"); state.disabled = true; Ok(output(0, Vec::new(), b"")) }
            "bootout" => {
                state.events.borrow_mut().push("bootout");
                if state.fault == "bootout" { return Err(std::io::Error::other("synthetic bootout failed")); }
                state.loaded = false; Ok(output(0, Vec::new(), b""))
            }
            "bootstrap" => {
                state.events.borrow_mut().push("bootstrap");
                if state.disabled || state.loaded { return Ok(output(1, Vec::new(), b"model bootstrap refused")); }
                state.loaded = true; Ok(output(0, Vec::new(), b""))
            }
            "kickstart" => {
                state.events.borrow_mut().push("kickstart");
                if !state.loaded { return Ok(output(1, Vec::new(), b"model job unloaded")); }
                state.spawn_stub()?; Ok(output(0, Vec::new(), b""))
            }
            _ => panic!("unexpected service action"),
        }
    })).expect("synthetic runner selection");
    let mut backend = InstalledServiceEffects::new(data.path(), ServiceBudgets::SCALED_TEST);
    let issuer = StubIssuer::valid();
    drop(reserve);
    let (result, output) = lifecycle_command_for_slot(&issuer, data.path(), &mut backend, fleet, Some(events));
    assert_eq!(result.is_ok(), fault.is_empty(), "production deadline forms: {fault}");
    assert_eq!(issuer.calls(), usize::from(fault.is_empty()));
    if fault.is_empty() {
        assert!(std::str::from_utf8(&output).expect("outcome").contains("signed_in"));
        let status = projection(data.path());
        assert_eq!(status.fleet_credentials.present, fleet);
        assert_eq!(status.legacy_credentials.present, !fleet);
    } else {
        assert!(output.is_empty(), "partial pause produces no authorization or grant output");
    }
    let state = state.borrow();
    assert!(state.loaded && !state.disabled);
    let operations = state.operations();
    assert!(!operations.contains(&"disable"));
    assert_eq!(
        operations.iter().filter(|op| **op == "bootstrap").count(),
        usize::from(fault != "bootout")
    );
    assert_eq!(
        operations.iter().filter(|op| **op == "kickstart").count(),
        1
    );
    assert!(operations.contains(&"plist_decode"));
    let child: serde_json::Value = serde_json::from_slice(
        &fs::read(state.data.join("stub-result.json")).expect("native child first admission"),
    )
    .expect("child JSON");
    assert_eq!(child["admitted"], true);
    drop(state);
    drop(hooks);
}

#[test]
fn ct100_transient_pause_reloads_job() {
    for fault in ["", "unload", "bootout", "persistent_pause"] {
        let data = root();
        let mut service = LifecycleService::new(data.path());
        service.fault = fault;
        let result = sign_in::with_sign_in_scope_for_test(
            &mut service,
            ServiceBudgets::SCALED_TEST,
            data.path(),
            true,
            |_| Ok::<(), CredentialCommandError>(()),
        );
        if fault == "persistent_pause" {
            assert!(
                matches!(result, Completion::RestartFailed { .. }),
                "M-PERSISTENT-STOP leaves the model disabled"
            );
            assert!(service.disabled && !service.loaded);
        } else {
            assert!(
                matches!(result, Completion::Finished(_)),
                "{fault}: {result:?}"
            );
            assert!(service.loaded && !service.disabled);
            assert!(!service.operations().contains(&"disable"));
            assert_eq!(
                service
                    .operations()
                    .iter()
                    .filter(|op| **op == "bootstrap")
                    .count(),
                usize::from(fault != "bootout")
            );
            assert_eq!(
                service
                    .operations()
                    .iter()
                    .filter(|op| **op == "kickstart")
                    .count(),
                1
            );
        }
    }
    #[cfg(not(feature = "credential-relocation-fixture"))]
    for fleet in [false, true] {
    for fault in ["", "unload", "bootout"] {
        native_lifecycle_case(fault, fleet);
    }
    }
}
