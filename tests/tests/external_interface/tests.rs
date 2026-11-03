use crate::external_interface::ExternalInterfaceTestProvider;
use ruffle_core::external::{CallbackError, Value as ExternalValue};
use ruffle_test_framework::environment::Environment;
use ruffle_test_framework::options::TestOptions;
use ruffle_test_framework::runner::TestStatus;
use ruffle_test_framework::test::Test;
use ruffle_test_framework::vfs::{PhysicalFS, VfsPath};
use std::collections::BTreeMap;
use std::thread::sleep;

pub fn external_interface_callback_result(
    environment: &impl Environment,
    vm: &str,
) -> Result<(), libtest_mimic::Failed> {
    let root = VfsPath::new(PhysicalFS::new(format!(
        "tests/swfs/{vm}/external_interface_callback_result/"
    )));
    let test = Test::from_options(
        TestOptions::read(&root.join("options.toml")?)?,
        root,
        format!("external_interface_callback_result_{vm}"),
    )?;
    let mut runner = test.create_test_runner(environment)?;
    runner
        .player()
        .lock()
        .unwrap()
        .set_external_interface_provider(Some(Box::new(ExternalInterfaceTestProvider::new())));

    let mut checked = false;
    loop {
        match runner.tick()? {
            TestStatus::Continue => {}
            TestStatus::Sleep(duration) => sleep(duration),
            TestStatus::Finished => break,
        }
        if !runner.is_preloaded() || checked {
            continue;
        }
        checked = true;
        runner
            .player()
            .lock()
            .unwrap()
            .mutate_with_update_context(|context| {
                let mut call = |name: &str, args: Vec<ExternalValue>| {
                    context
                        .external_interface
                        .get_callback(name)
                        .unwrap()
                        .call_result(context, name, args)
                };
                assert_eq!(
                    call("returnValue", vec![ExternalValue::Null]),
                    Ok(ExternalValue::Null)
                );
                assert_eq!(
                    call("returnValue", vec![ExternalValue::Undefined]),
                    Ok(ExternalValue::Undefined)
                );
                assert_eq!(
                    call("returnValue", vec!["reply".into()]),
                    Ok("reply".into())
                );
                let Err(CallbackError::Thrown(message)) =
                    call("throwString", vec!["callback failed".into()])
                else {
                    panic!("Expected a thrown callback error");
                };
                assert_eq!(message.lines().next(), Some("callback failed"));
                if vm == "avm1" {
                    assert_eq!(message, "callback failed");
                    assert_eq!(
                        call("throwUnprintable", vec![]),
                        Err(CallbackError::Thrown("[type Object]".into()))
                    );
                } else {
                    assert!(message.contains("\n\tat "), "Missing stack: {message}");
                    let Err(CallbackError::Thrown(message)) = call("throwError", vec![]) else {
                        panic!("Expected a thrown Error object");
                    };
                    assert_eq!(message.lines().next(), Some("Error: error object failed"));
                    assert!(message.contains("\n\tat "), "Missing stack: {message}");
                    assert_eq!(
                        call("throwUnprintable", vec![]),
                        Err(CallbackError::Thrown(
                            "<failed to display AVM error>".into()
                        ))
                    );
                }
                if vm == "avm2" {
                    assert_eq!(call("uncaughtCount", vec![]), Ok(0.into()));
                } else {
                    assert_eq!(
                        call("returnBadValue", vec![]),
                        Err(CallbackError::ReplyConversion("conversion failed".into()))
                    );
                }
                let callback = context
                    .external_interface
                    .get_callback("throwString")
                    .unwrap();
                assert_eq!(
                    callback.call(context, "throwString", ["legacy failed".into()]),
                    ExternalValue::Null
                );
                if vm == "avm2" {
                    let callback = context
                        .external_interface
                        .get_callback("uncaughtCount")
                        .unwrap();
                    assert_eq!(
                        callback.call_result(context, "uncaughtCount", []),
                        Ok(1.into())
                    );
                } else {
                    let callback = context
                        .external_interface
                        .get_callback("returnBadValue")
                        .unwrap();
                    assert_eq!(
                        callback.call(context, "returnBadValue", []),
                        ExternalValue::Null
                    );
                    let callback = context
                        .external_interface
                        .get_callback("throwUnprintable")
                        .unwrap();
                    assert_eq!(
                        callback.call(context, "throwUnprintable", []),
                        ExternalValue::Null
                    );
                    let counter = context
                        .external_interface
                        .get_callback("stringificationCount")
                        .unwrap();
                    assert_eq!(
                        counter.call_result(context, "stringificationCount", []),
                        Ok(1.into())
                    );
                }
            });
    }
    assert!(checked, "Callbacks were never exercised");
    Ok(())
}

pub fn external_interface_avm1(
    environment: &impl Environment,
) -> Result<(), libtest_mimic::Failed> {
    let test = &Test::from_options(
        TestOptions {
            num_frames: Some(2),
            ..Default::default()
        },
        VfsPath::new(PhysicalFS::new("tests/swfs/avm1/external_interface/")),
        "external_interface_avm1".to_string(),
    )?;
    let mut runner = test.create_test_runner(environment)?;

    runner
        .player()
        .lock()
        .unwrap()
        .set_external_interface_provider(Some(Box::new(ExternalInterfaceTestProvider::new())));

    let mut first = true;

    loop {
        match runner.tick()? {
            TestStatus::Continue => {}
            TestStatus::Sleep(duration) => sleep(duration),
            TestStatus::Finished => break,
        }

        if !runner.is_preloaded() {
            continue;
        }

        if first {
            first = false;
            let mut player_locked = runner.player().lock().unwrap();

            let parroted = player_locked.call_internal_interface("parrot", ["Hello World!".into()]);
            player_locked.log_backend().avm_trace(&format!(
                "After calling `parrot` with a string: {parroted:?}",
            ));

            let mut nested = BTreeMap::new();
            nested.insert(
                "list".to_string(),
                vec![
                    "string".into(),
                    100.into(),
                    false.into(),
                    ExternalValue::Object(BTreeMap::new()),
                ]
                .into(),
            );

            let mut root = BTreeMap::new();
            root.insert("number".to_string(), (-500.1).into());
            root.insert("string".to_string(), "A string!".into());
            root.insert("true".to_string(), true.into());
            root.insert("false".to_string(), false.into());
            root.insert("null".to_string(), ExternalValue::Null);
            root.insert("nested".to_string(), nested.into());
            let result =
                player_locked.call_internal_interface("callWith", ["trace".into(), root.into()]);
            player_locked.log_backend().avm_trace(&format!(
                "After calling `callWith` with a complex payload: {result:?}",
            ));
        }
    }

    Ok(())
}

pub fn external_interface_avm2(
    environment: &impl Environment,
) -> Result<(), libtest_mimic::Failed> {
    let test = &Test::from_options(
        TestOptions {
            num_frames: Some(2),
            ..Default::default()
        },
        VfsPath::new(PhysicalFS::new("tests/swfs/avm2/external_interface/")),
        "external_interface_avm2".to_string(),
    )?;
    let mut runner = test.create_test_runner(environment)?;
    runner
        .player()
        .lock()
        .unwrap()
        .set_external_interface_provider(Some(Box::new(ExternalInterfaceTestProvider::new())));

    let mut first = true;

    loop {
        match runner.tick()? {
            TestStatus::Continue => {}
            TestStatus::Sleep(duration) => sleep(duration),
            TestStatus::Finished => break,
        }

        if !runner.is_preloaded() {
            continue;
        }

        if first {
            first = false;
            let mut player_locked = runner.player().lock().unwrap();

            let parroted = player_locked.call_internal_interface("parrot", ["Hello World!".into()]);
            player_locked.log_backend().avm_trace(&format!(
                "After calling `parrot` with a string: {parroted:?}",
            ));

            let map = BTreeMap::from([("a".into(), 100.into()), ("b".into(), "string".into())]);
            let parroted =
                player_locked.call_internal_interface("parrot", [ExternalValue::Object(map)]);
            player_locked.log_backend().avm_trace(&format!(
                "After calling `parrot` with an object: {parroted:?}",
            ));

            player_locked.call_internal_interface("freestanding", ["Hello World!".into()]);

            player_locked
                .log_backend()
                .avm_trace("Before calling `removeMe`");
            let removed = player_locked.call_internal_interface("removeMe", []);
            player_locked
                .log_backend()
                .avm_trace(&format!("After calling `removeMe`: {removed:?}",));

            let root: ExternalValue = vec![
                "string".into(),
                100.into(),
                ExternalValue::Null,
                false.into(),
            ]
            .into();

            let result = player_locked.call_internal_interface("callWith", ["trace".into(), root]);
            player_locked.log_backend().avm_trace(&format!(
                "After calling `callWith` with a complex payload: {result:?}",
            ));
        }
    }

    Ok(())
}
