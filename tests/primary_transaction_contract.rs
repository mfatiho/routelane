use routelane::backend::{
    InterfaceIdentity, InterfaceSettingReceipt, MarkAwareRpf, MarkAwareRpfBackend, PrivilegedError,
};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct FakeSettings(Arc<Mutex<State>>);
struct State {
    identity: InterfaceIdentity,
    current: u8,
    global_rpf: u8,
    interface_rpf: u8,
    global_mark: u8,
    log: Vec<&'static str>,
    writes: usize,
}

fn identity() -> InterfaceIdentity {
    InterfaceIdentity {
        ifindex: 2,
        name: "eth0".into(),
        kind: "ether".into(),
        hardware_address: Some("02:00:00:00:00:01".into()),
    }
}
impl FakeSettings {
    fn strict() -> Self {
        Self(Arc::new(Mutex::new(State {
            identity: identity(),
            current: 0,
            global_rpf: 1,
            interface_rpf: 1,
            global_mark: 0,
            log: vec![],
            writes: 0,
        })))
    }
}
impl MarkAwareRpfBackend for FakeSettings {
    fn identity(&mut self, _: &str) -> Result<InterfaceIdentity, PrivilegedError> {
        Ok(self.0.lock().unwrap().identity.clone())
    }
    fn rp_filter(&mut self, name: &str) -> Result<u8, PrivilegedError> {
        let s = self.0.lock().unwrap();
        Ok(if name == "all" {
            s.global_rpf
        } else {
            s.interface_rpf
        })
    }
    fn src_valid_mark(&mut self, name: &str) -> Result<u8, PrivilegedError> {
        let s = self.0.lock().unwrap();
        Ok(if name == "all" {
            s.global_mark
        } else {
            s.current
        })
    }
    fn set_src_valid_mark(
        &mut self,
        who: &InterfaceIdentity,
        value: u8,
    ) -> Result<(), PrivilegedError> {
        assert_eq!(
            who.name, "eth0",
            "never write global/default/foreign interfaces"
        );
        assert!(value <= 1);
        let mut s = self.0.lock().unwrap();
        s.current = value;
        s.writes += 1;
        s.log.push("write");
        Ok(())
    }
}

#[test]
fn interface_setting_is_journaled_before_write() {
    let fake = FakeSettings::strict();
    let log = Arc::clone(&fake.0);
    let mut denied = MarkAwareRpf::new(fake.clone(), move |_: &InterfaceSettingReceipt| {
        log.lock().unwrap().log.push("journal-failed");
        Err(PrivilegedError::new("fsync failed"))
    });
    assert!(denied.ensure_mark_aware_rpf(&identity()).is_err());
    assert_eq!(
        fake.0.lock().unwrap().writes,
        0,
        "failed journal fsync must mean zero setting writes"
    );
    let log = Arc::clone(&fake.0);
    let mut settings = MarkAwareRpf::new(fake.clone(), move |receipt: &InterfaceSettingReceipt| {
        assert_eq!((receipt.previous, receipt.applied), (0, 1));
        log.lock().unwrap().log.push("journal");
        Ok(())
    });
    let receipt = settings
        .ensure_mark_aware_rpf(&identity())
        .unwrap()
        .unwrap();
    assert_eq!(receipt.identity, identity());
    let state = fake.0.lock().unwrap();
    assert_eq!(state.log, vec!["journal-failed", "journal", "write"]);
    assert_eq!(
        (
            state.current,
            state.global_rpf,
            state.interface_rpf,
            state.global_mark
        ),
        (1, 1, 1, 0)
    );
}

#[test]
fn foreign_setting_change_is_not_restored() {
    let fake = FakeSettings::strict();
    let mut settings = MarkAwareRpf::new(fake.clone(), |_: &InterfaceSettingReceipt| Ok(()));
    let receipt = settings
        .ensure_mark_aware_rpf(&identity())
        .unwrap()
        .unwrap();
    let administrator_setting = 0;
    fake.0.lock().unwrap().current = administrator_setting;
    let before = fake.0.lock().unwrap().writes;
    settings.restore_mark_aware_rpf(&receipt).unwrap();
    let state = fake.0.lock().unwrap();
    assert_eq!(state.current, administrator_setting);
    assert_eq!(state.writes, before);
}

#[test]
fn interface_replacement_after_journal_cannot_receive_a_setting_write() {
    let fake = FakeSettings::strict();
    let state = Arc::clone(&fake.0);
    let mut settings = MarkAwareRpf::new(fake.clone(), move |_: &InterfaceSettingReceipt| {
        state.lock().unwrap().identity.ifindex = 9;
        Ok(())
    });
    assert!(settings.ensure_mark_aware_rpf(&identity()).is_err());
    assert_eq!(fake.0.lock().unwrap().writes, 0);
}

#[test]
fn loose_rpf_or_global_mark_awareness_needs_no_interface_write() {
    for (rpf, mark) in [(2, 0), (1, 1), (0, 0)] {
        let fake = FakeSettings::strict();
        {
            let mut s = fake.0.lock().unwrap();
            s.global_rpf = rpf;
            s.interface_rpf = rpf;
            s.global_mark = mark;
        }
        let mut settings = MarkAwareRpf::new(fake.clone(), |_: &InterfaceSettingReceipt| {
            panic!("unneeded setting must not be journaled")
        });
        assert!(settings
            .ensure_mark_aware_rpf(&identity())
            .unwrap()
            .is_none());
        assert_eq!(fake.0.lock().unwrap().writes, 0);
    }
}

#[test]
fn recorded_setting_is_restored_only_on_original_interface_identity() {
    let fake = FakeSettings::strict();
    let mut settings = MarkAwareRpf::new(fake.clone(), |_: &InterfaceSettingReceipt| Ok(()));
    let receipt = settings
        .ensure_mark_aware_rpf(&identity())
        .unwrap()
        .unwrap();
    settings.restore_mark_aware_rpf(&receipt).unwrap();
    assert_eq!(fake.0.lock().unwrap().current, 0);
    assert_eq!(fake.0.lock().unwrap().writes, 2);
    {
        let mut state = fake.0.lock().unwrap();
        state.current = 1;
        state.identity.ifindex = 9;
    }
    assert!(settings.restore_mark_aware_rpf(&receipt).is_err());
    assert_eq!(fake.0.lock().unwrap().current, 1);
    assert_eq!(fake.0.lock().unwrap().writes, 2);
}

#[test]
fn global_default_and_path_like_names_cannot_authorize_a_setting_write() {
    let fake = FakeSettings::strict();
    let mut settings = MarkAwareRpf::new(fake.clone(), |_: &InterfaceSettingReceipt| {
        panic!("unsafe interface must not reach the journal")
    });
    for name in ["all", "default", "../all", "eth0/src_valid_mark", ""] {
        let mut invalid = identity();
        invalid.name = name.into();
        assert!(settings.ensure_mark_aware_rpf(&invalid).is_err());
    }
    assert_eq!(fake.0.lock().unwrap().writes, 0);
}
