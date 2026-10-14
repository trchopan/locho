use jni::{
    objects::{GlobalRef, JClass, JObject},
    JNIEnv,
};
use std::sync::Mutex;
use std::sync::OnceLock;

static CONTEXT: OnceLock<GlobalRef> = OnceLock::new();
static INITIALIZE: Mutex<()> = Mutex::new(());
pub fn initialized() -> bool {
    CONTEXT.get().is_some()
}

#[no_mangle]
pub extern "system" fn Java_dev_chop_locho_LochoFlutterPlugin_initializeContext(
    env: JNIEnv,
    _class: JClass,
    context: JObject,
) -> jni::sys::jboolean {
    let Ok(_guard) = INITIALIZE.lock() else {
        return 0;
    };
    if initialized() {
        return 1;
    }
    let result = (|| {
        let mut unowned = unsafe { jni_new::EnvUnowned::from_raw(env.get_raw().cast()) };
        let raw_context = context.as_raw();
        let verified = unowned.with_env(|environment| {
            let context = unsafe { jni_new::objects::JObject::from_raw(environment, raw_context) };
            rustls_platform_verifier::android::init_with_env(environment, context)?;
            Ok::<bool, jni_new::errors::Error>(true)
        });
        if !verified.resolve::<jni_new::errors::ThrowRuntimeExAndDefault>() {
            return None;
        }
        let vm = env.get_java_vm().ok()?;
        let global = env.new_global_ref(context).ok()?;
        // Plugin initialization runs once on Android's main thread. Keep the
        // application GlobalRef for process lifetime, as required by iroh DNS.
        let context = CONTEXT.get_or_init(|| global);
        unsafe {
            iroh::dns::install_android_jni_context(
                vm.get_java_vm_pointer().cast(),
                context.as_obj().as_raw().cast(),
            );
        }
        Some(())
    })();
    u8::from(result.is_some())
}
