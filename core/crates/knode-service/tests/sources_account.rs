//! Signing a connector in over `sources.account`, `sources.sign_in` and `sources.sign_out`,
//! and signing it out when it is disconnected from the last space. The fake connector keeps
//! its own state, so no keychain is touched.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use knode_format::{Provenance, Space};
use knode_service::sources::{
    Connector, Cursor, FetchRequest, Fetched, Permission, Secrets, SignInState, SignInStep,
    SourceError, SourceInfo, SourcesOptions,
};
use knode_service::{Client, Config, ServiceHandle, start};
use tempfile::TempDir;

const ID: &str = "chatty";

#[derive(Default)]
struct Account {
    code_sent: bool,
    signed_in: bool,
    sign_outs: usize,
    /// While set, the next sign-in step waits until a message comes through it, like a slow
    /// network.
    hold: Option<std::sync::mpsc::Receiver<()>>,
    /// Whether a sign-in step is waiting on `hold` now.
    waiting: bool,
}

#[derive(Clone, Default)]
struct Chatty(Arc<Mutex<Account>>);

impl Connector for Chatty {
    fn info(&self) -> SourceInfo {
        SourceInfo::connector(ID, "Chatty", "A fake chat service").needing([Permission::SignIn])
    }

    fn fetch(&self, request: &FetchRequest<'_>) -> Result<Fetched, SourceError> {
        if !self.0.lock().unwrap().signed_in {
            return Err(SourceError::SignIn("sign in first".into()));
        }
        Ok(Fetched::nothing(
            request.cursor.cloned().unwrap_or_else(|| Cursor::new("0")),
        ))
    }

    fn sign_in_state(&self, _secrets: &dyn Secrets) -> Result<SignInState, SourceError> {
        let a = self.0.lock().unwrap();
        Ok(if a.signed_in {
            SignInState::SignedIn {
                account: "Ada".into(),
            }
        } else if a.code_sent {
            SignInState::CodeSent { via: None }
        } else {
            SignInState::SignedOut
        })
    }

    fn sign_in(
        &self,
        step: SignInStep,
        _secrets: &dyn Secrets,
    ) -> Result<SignInState, SourceError> {
        let hold = {
            let mut a = self.0.lock().unwrap();
            a.waiting = a.hold.is_some();
            a.hold.take()
        };
        if let Some(hold) = hold {
            let _ = hold.recv();
            self.0.lock().unwrap().waiting = false;
        }
        let mut a = self.0.lock().unwrap();
        match step {
            SignInStep::Start { .. } => {
                a.code_sent = true;
                Ok(SignInState::CodeSent {
                    via: Some("the app".into()),
                })
            }
            SignInStep::Code { code } if a.code_sent && code == "123" => {
                a.signed_in = true;
                Ok(SignInState::SignedIn {
                    account: "Ada".into(),
                })
            }
            _ => Err(SourceError::SignIn("that code is not right".into())),
        }
    }

    fn sign_out(&self, _secrets: &dyn Secrets) -> Result<bool, SourceError> {
        let mut a = self.0.lock().unwrap();
        a.sign_outs += 1;
        let was = a.signed_in;
        a.signed_in = false;
        a.code_sent = false;
        Ok(was)
    }
}

struct Fixture {
    dir: TempDir,
    fake: Chatty,
    _service: ServiceHandle,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for name in ["Personal", "Work"] {
            drop(Space::create(dir.path().join(format!("{name}.knode")), name).unwrap());
        }
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let personal = fs::canonicalize(dir.path().join("Personal.knode")).unwrap();
        fs::write(
            home.join("config.toml"),
            format!("default_space = {:?}\n", personal.to_str().unwrap()),
        )
        .unwrap();
        let fake = Chatty::default();
        let options = SourcesOptions::with_connectors(vec![Arc::new(fake.clone())]);
        let service = start(Config::new(&home).with_sources(options)).unwrap();
        Fixture {
            dir,
            fake,
            _service: service,
        }
    }

    fn client(&self) -> Client {
        Client::connect(self.dir.path().join("home/service.sock")).unwrap()
    }

    fn space(&self, name: &str) -> PathBuf {
        fs::canonicalize(self.dir.path().join(format!("{name}.knode"))).unwrap()
    }
}

fn me() -> Provenance {
    Provenance::person("example")
}

#[test]
fn a_connector_signs_in_step_by_step() {
    let f = Fixture::new();
    let client = f.client();
    assert_eq!(client.sources_account(ID).unwrap(), SignInState::SignedOut);
    let sent = client
        .sources_sign_in(
            ID,
            SignInStep::Start {
                account: "+34600000000".into(),
            },
        )
        .unwrap();
    assert_eq!(
        sent,
        SignInState::CodeSent {
            via: Some("the app".into())
        }
    );
    let wrong = client
        .sources_sign_in(ID, SignInStep::Code { code: "999".into() })
        .unwrap_err();
    assert!(wrong.to_string().contains("not right"), "{wrong}");
    let done = client
        .sources_sign_in(ID, SignInStep::Code { code: "123".into() })
        .unwrap();
    assert_eq!(
        done,
        SignInState::SignedIn {
            account: "Ada".into()
        }
    );
    assert!(client.sources_sign_out(ID).unwrap());
    assert_eq!(client.sources_account(ID).unwrap(), SignInState::SignedOut);
    // A source with no such connector.
    assert!(client.sources_account("nothing").is_err());
}

#[test]
fn disconnecting_the_last_space_signs_out() {
    let f = Fixture::new();
    let client = f.client();
    f.fake.0.lock().unwrap().signed_in = true;
    let (personal, work) = (f.space("Personal"), f.space("Work"));
    // Work is not in config.toml: opening it makes the service know it.
    client.sources_list(&work).unwrap();
    client.sources_connect(&personal, ID, None, &me()).unwrap();
    client.sources_connect(&work, ID, None, &me()).unwrap();

    assert!(client.sources_disconnect(&personal, ID, &me()).unwrap());
    assert_eq!(
        f.fake.0.lock().unwrap().sign_outs,
        0,
        "still connected to another space"
    );
    assert!(client.sources_disconnect(&work, ID, &me()).unwrap());
    let account = f.fake.0.lock().unwrap();
    assert_eq!(account.sign_outs, 1);
    assert!(!account.signed_in);
}

#[test]
fn a_slow_sign_in_step_does_not_block_the_space() {
    let f = Fixture::new();
    let (release, hold) = std::sync::mpsc::channel();
    f.fake.0.lock().unwrap().hold = Some(hold);
    let socket = f.dir.path().join("home/service.sock");
    let signing = std::thread::spawn(move || {
        Client::connect(socket).unwrap().sources_sign_in(
            ID,
            SignInStep::Start {
                account: "+34600000000".into(),
            },
        )
    });
    let start = std::time::Instant::now();
    while !f.fake.0.lock().unwrap().waiting {
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // While the step waits for its network, the space answers other calls.
    let (done, answered) = std::sync::mpsc::channel();
    let socket = f.dir.path().join("home/service.sock");
    let space = f.space("Personal");
    std::thread::spawn(move || {
        let client = Client::connect(socket).unwrap();
        let listed = client.sources_list(&space).is_ok();
        let connected = client.sources_connect(&space, ID, None, &me()).is_ok();
        let _ = done.send(listed && connected);
    });
    let ok = answered
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the space answered while a sign-in step was waiting");
    assert!(ok);
    release.send(()).unwrap();
    let sent = signing.join().unwrap().unwrap();
    assert!(matches!(sent, SignInState::CodeSent { .. }));
}
