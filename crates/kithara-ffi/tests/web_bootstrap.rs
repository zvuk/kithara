#![cfg(target_arch = "wasm32")]

use admission::Admission;
use kithara::platform::{
    thread,
    time::{Duration, timeout},
};
use kithara_ffi::{player::AudioPlayer, web::bindings};
use kithara_test_utils::wait_until;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
async fn worklet_import_waits_for_worker_bootstrap_admission() {
    assert!(thread::is_main_thread());
    thread::set_wasm_shim_name("wasm-bindgen-test");
    let mut admission = Admission::hold()
        .await
        .expect("hold bootstrap admission before host construction");
    let setup = bindings::setup();
    let player = AudioPlayer::new_js();
    let command = player.set_volume_js(0.25);
    let observed = wait_until(Duration::from_secs(5), "worklet admission", || {
        admission.worklet_imports() > 0 || admission.bootstrap_requests() > 0
    })
    .await;
    let imported = admission.worklet_imports();
    let requested = admission.bootstrap_requests();
    admission
        .release()
        .await
        .expect("release bootstrap admission");

    setup.expect("browser bindings initialize");
    command.expect("volume command admitted");
    observed.expect("worklet attempts module admission");
    assert_eq!(imported, 0, "worklet import must wait for the held lease");
    assert_eq!(requested, 1, "one host requests one worklet module load");

    let document = web_sys::window()
        .expect("browser window")
        .document()
        .expect("browser document");
    let ready = wait_until(Duration::from_secs(5), "owner after bootstrap", || {
        player.tick_js();
        let click = web_sys::Event::new("click").expect("click event");
        document.dispatch_event(&click).expect("dispatch click");
        player.volume() == 0.25 && player.rt_process_calls_js() > 0
    })
    .await;
    let volume = player.volume();
    let mixer_blocks = player.rt_process_calls_js();
    let started_within_lease = admission.processor_started_within_lease();
    let contexts = admission.audio_contexts();
    // A stuck snapshot query must not hold up reporting a readiness failure.
    let locks = timeout(Duration::from_millis(100), admission.bootstrap_locks()).await;
    wasm_bindgen_test::console_log!(
        "BOOTSTRAP_READY volume={} mixer_blocks={} audio_contexts={} bootstrap_locks={locks:?}",
        volume,
        mixer_blocks,
        contexts,
    );
    ready.expect("the real mixer and owner run after admission is released");
    // A wasm thread start that contends with another one waits on shared
    // memory, which traps in an AudioWorklet, so the processor starts its
    // thread before the worklet gives the bootstrap admission back.
    assert!(
        started_within_lease,
        "the worklet processor starts its thread within the bootstrap admission",
    );
}

mod admission {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    use js_sys::{Array, Function, Object, Promise, Proxy, Reflect, futures::JsFuture};
    use wasm_bindgen::prelude::*;

    const KEY: &str = "wasm_safe_thread:bootstrap";
    type Events = Rc<RefCell<Vec<String>>>;

    #[derive(Default)]
    struct Observation {
        imports: Cell<u32>,
        requests: Cell<u32>,
        bootstrap: Events,
        contexts: RefCell<Vec<Context>>,
        hooks: RefCell<Vec<Property>>,
    }

    struct Context {
        worklet: JsValue,
        context: JsValue,
        transitions: Events,
    }

    pub(super) struct Admission {
        observation: Rc<Observation>,
        locks: JsValue,
        held: Promise,
        release: Option<Function>,
        release_hooks: Vec<Property>,
    }

    impl Admission {
        pub(super) async fn hold() -> Result<Self, JsValue> {
            let locks = web_sys::window()
                .expect("browser window")
                .navigator()
                .locks();
            let mut release = None;
            let gate = Promise::new(&mut |resolve, _| release = Some(resolve));
            let mut acquired = None;
            let ready = Promise::new(&mut |resolve, _| acquired = Some(resolve));
            let acquired = acquired.expect("promise executor runs synchronously");
            let callback = Closure::once_into_js(move |_: JsValue| -> Result<JsValue, JsValue> {
                acquired.call0(&JsValue::UNDEFINED)?;
                Ok(gate.into())
            });
            let held = function(locks.as_ref(), "request")?
                .call2(locks.as_ref(), &KEY.into(), &callback)?
                .dyn_into()?;
            let mut admission = Self {
                observation: Rc::default(),
                locks: locks.into(),
                held,
                release,
                release_hooks: Vec::new(),
            };
            JsFuture::from(ready).await?;
            admission.install()?;
            Ok(admission)
        }

        fn install(&mut self) -> Result<(), JsValue> {
            let state = Rc::clone(&self.observation);
            let request = proxy(
                &function(&self.locks, "request")?.into(),
                "apply",
                move |target, receiver, args| {
                    let args: Array = args.unchecked_into();
                    if args.get(0).as_string().as_deref() == Some(KEY) {
                        state.requests.set(state.requests.get() + 1);
                        let events = Rc::clone(&state.bootstrap);
                        let index = args.length() - 1;
                        let callback =
                            proxy(&args.get(index), "apply", move |target, receiver, args| {
                                events.borrow_mut().push("lock-granted".into());
                                apply(&target, &receiver, &args)
                            })?;
                        args.set(index, callback);
                        let promise = apply(&target, &receiver, args.as_ref())?;
                        observe(&state.bootstrap, "lock", &promise)?;
                        Ok(promise)
                    } else {
                        apply(&target, &receiver, args.as_ref())
                    }
                },
            )?;
            self.release_hooks
                .push(Property::value(&self.locks, "request", &request)?);
            self.install_worklets()?;
            Ok(())
        }

        fn install_worklets(&mut self) -> Result<(), JsValue> {
            let global = js_sys::global();
            let constructor = get(&global, "AudioWorkletNode")?;
            let events = Rc::clone(&self.observation.bootstrap);
            let node = proxy(
                &constructor,
                "construct",
                move |target, args, new_target| {
                    let node = Reflect::construct_with_new_target(
                        target.unchecked_ref(),
                        args.unchecked_ref(),
                        new_target.unchecked_ref(),
                    )?;
                    events.borrow_mut().push("node-constructed".into());
                    let started = Rc::clone(&events);
                    let listener = Closure::once_into_js(move |_: JsValue| {
                        started.borrow_mut().push("processor-started".into());
                    });
                    let options = Object::new();
                    Reflect::set(&options, &"once".into(), &JsValue::TRUE)?;
                    let port = get(&node, "port")?;
                    function(&port, "addEventListener")?.call3(
                        &port,
                        &"message".into(),
                        &listener,
                        &options,
                    )?;
                    Ok(node)
                },
            )?;
            self.observation.hooks.borrow_mut().push(Property::value(
                &global,
                "AudioWorkletNode",
                &node,
            )?);
            let prototype = get(&get(&global, "BaseAudioContext")?, "prototype")?;
            let descriptor = Reflect::get_own_property_descriptor(
                prototype.unchecked_ref::<Object>(),
                &"audioWorklet".into(),
            )?;
            let state = Rc::clone(&self.observation);
            let getter = proxy(
                &get(&descriptor, "get")?,
                "apply",
                move |target, context, args| {
                    let worklet = apply(&target, &context, &args)?;
                    state.instrument(&worklet, &context)?;
                    Ok(worklet)
                },
            )?;
            let replacement = Object::assign(&Object::new(), descriptor.unchecked_ref::<Object>());
            Reflect::set(&replacement, &"get".into(), &getter)?;
            self.release_hooks
                .push(Property::install(&prototype, "audioWorklet", &replacement)?);
            Ok(())
        }

        pub(super) async fn release(&mut self) -> Result<(), JsValue> {
            restore(&mut self.release_hooks)?;
            self.release
                .take()
                .expect("held bootstrap lease")
                .call0(&JsValue::UNDEFINED)?;
            JsFuture::from(self.held.clone()).await?;
            Ok(())
        }

        pub(super) fn worklet_imports(&self) -> u32 {
            self.observation.imports.get()
        }
        pub(super) fn bootstrap_requests(&self) -> u32 {
            self.observation.requests.get()
        }

        pub(super) fn processor_started_within_lease(&self) -> bool {
            let events = self.observation.bootstrap.borrow();
            let started = events.iter().position(|event| event == "processor-started");
            let released = events.iter().position(|event| event == "lock-resolved");
            matches!((started, released), (Some(started), Some(released)) if started < released)
        }

        pub(super) fn audio_contexts(&self) -> String {
            let contexts = Array::new();
            for entry in self.observation.contexts.borrow().iter() {
                let snapshot = Object::new();
                for name in ["state", "currentTime", "sampleRate"] {
                    Reflect::set(
                        &snapshot,
                        &name.into(),
                        &get(&entry.context, name).expect("audio context property"),
                    )
                    .expect("snapshot property");
                }
                for (name, events) in [
                    ("bootstrap", &self.observation.bootstrap),
                    ("transitions", &entry.transitions),
                ] {
                    let values: Array = events
                        .borrow()
                        .iter()
                        .map(|event| JsValue::from_str(event))
                        .collect();
                    Reflect::set(&snapshot, &name.into(), &values).expect("snapshot events");
                }
                contexts.push(&snapshot);
            }
            let snapshot = js_sys::JSON::stringify(&contexts)
                .expect("context snapshot JSON")
                .into();
            restore(&mut self.observation.hooks.borrow_mut())
                .expect("restore browser bootstrap hooks");
            snapshot
        }

        pub(super) async fn bootstrap_locks(&self) -> Result<JsValue, JsValue> {
            let snapshot = JsFuture::from(
                function(&self.locks, "query")?
                    .call0(&self.locks)?
                    .dyn_into::<Promise>()?,
            )
            .await?;
            let selected = Object::new();
            for name in ["held", "pending"] {
                let locks: Array = get(&snapshot, name)?.dyn_into()?;
                let ours = Array::new();
                for lock in locks.iter() {
                    if get(&lock, "name")?.as_string().as_deref() == Some(KEY) {
                        ours.push(&lock);
                    }
                }
                Reflect::set(&selected, &name.into(), &ours)?;
            }
            Ok(js_sys::JSON::stringify(&selected)?.into())
        }
    }

    impl Drop for Admission {
        fn drop(&mut self) {
            for result in [
                restore(&mut self.release_hooks),
                restore(&mut self.observation.hooks.borrow_mut()),
            ] {
                if let Err(error) = result {
                    wasm_bindgen_test::console_error!("restore bootstrap observation: {error:?}");
                }
            }
            if let Some(release) = self.release.take()
                && let Err(error) = release.call0(&JsValue::UNDEFINED)
            {
                wasm_bindgen_test::console_error!("release bootstrap lease: {error:?}");
            }
        }
    }

    impl Observation {
        fn instrument(
            self: &Rc<Self>,
            worklet: &JsValue,
            context: &JsValue,
        ) -> Result<(), JsValue> {
            if self
                .contexts
                .borrow()
                .iter()
                .any(|entry| entry.worklet == *worklet)
            {
                return Ok(());
            }
            let events: Events = Rc::default();
            for (receiver, name) in [(worklet, "addModule"), (context, "resume")] {
                let state = Rc::clone(self);
                let events = Rc::clone(&events);
                let method = proxy(
                    &function(receiver, name)?.into(),
                    "apply",
                    move |target, receiver, args| {
                        if name == "addModule" {
                            state.imports.set(state.imports.get() + 1);
                        }
                        events.borrow_mut().push(format!("{name}-called"));
                        let result = apply(&target, &receiver, &args);
                        match &result {
                            Ok(promise) => observe(&events, name, promise)?,
                            Err(error) => {
                                events.borrow_mut().push(format!("{name}-threw: {error:?}"))
                            }
                        }
                        result
                    },
                )?;
                self.hooks
                    .borrow_mut()
                    .push(Property::value(receiver, name, &method)?);
            }
            self.contexts.borrow_mut().push(Context {
                worklet: worklet.clone(),
                context: context.clone(),
                transitions: events,
            });
            Ok(())
        }
    }

    struct Property {
        object: Object,
        name: JsValue,
        original: JsValue,
    }

    impl Property {
        fn value(object: &JsValue, name: &str, value: &JsValue) -> Result<Self, JsValue> {
            let original = Reflect::get_own_property_descriptor(
                object.unchecked_ref::<Object>(),
                &name.into(),
            )?;
            let descriptor = if original.is_undefined() {
                let descriptor = Object::new();
                for flag in ["writable", "enumerable", "configurable"] {
                    Reflect::set(&descriptor, &flag.into(), &JsValue::TRUE)?;
                }
                descriptor
            } else {
                Object::assign(&Object::new(), original.unchecked_ref::<Object>())
            };
            Reflect::set(&descriptor, &"value".into(), value)?;
            Self::install(object, name, &descriptor)
        }

        fn install(object: &JsValue, name: &str, descriptor: &Object) -> Result<Self, JsValue> {
            let object: Object = object.clone().dyn_into()?;
            let name = JsValue::from_str(name);
            let original = Reflect::get_own_property_descriptor(&object, &name)?;
            if !Reflect::define_property(&object, &name, descriptor)? {
                return Err(JsValue::from_str("cannot install bootstrap observation"));
            }
            Ok(Self {
                object,
                name,
                original,
            })
        }
    }

    fn restore(properties: &mut Vec<Property>) -> Result<(), JsValue> {
        while let Some(property) = properties.pop() {
            let restored = if property.original.is_undefined() {
                Reflect::delete_property(&property.object, &property.name)
            } else {
                Reflect::define_property(
                    &property.object,
                    &property.name,
                    property.original.unchecked_ref(),
                )
            };
            if !restored? {
                return Err(JsValue::from_str("cannot restore bootstrap observation"));
            }
        }
        Ok(())
    }

    fn get(object: &JsValue, name: &str) -> Result<JsValue, JsValue> {
        Reflect::get(object, &name.into())
    }
    fn function(object: &JsValue, name: &str) -> Result<Function, JsValue> {
        get(object, name)?.dyn_into()
    }
    fn apply(target: &JsValue, receiver: &JsValue, args: &JsValue) -> Result<JsValue, JsValue> {
        Reflect::apply(
            target.unchecked_ref::<Function>(),
            receiver,
            args.unchecked_ref::<Array>(),
        )
    }

    fn proxy(
        target: &JsValue,
        kind: &str,
        callback: impl FnMut(JsValue, JsValue, JsValue) -> Result<JsValue, JsValue> + 'static,
    ) -> Result<JsValue, JsValue> {
        let handler = Object::new();
        let closure = Closure::wrap(Box::new(callback)
            as Box<dyn FnMut(JsValue, JsValue, JsValue) -> Result<JsValue, JsValue>>);
        Reflect::set(&handler, &kind.into(), &closure.into_js_value())?;
        Ok(Proxy::new(target, &handler).into())
    }

    fn observe(events: &Events, name: &'static str, promise: &JsValue) -> Result<(), JsValue> {
        let resolved = Rc::clone(events);
        let resolve = Closure::once_into_js(move |_: JsValue| {
            resolved.borrow_mut().push(format!("{name}-resolved"));
        });
        let rejected = Rc::clone(events);
        let reject = Closure::once_into_js(move |error: JsValue| {
            rejected
                .borrow_mut()
                .push(format!("{name}-rejected: {error:?}"));
        });
        function(promise, "then")?.call2(promise, &resolve, &reject)?;
        Ok(())
    }
}
