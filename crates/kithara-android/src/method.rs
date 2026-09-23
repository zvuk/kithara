use jni::{
    Env, JValue, JValueOwned,
    objects::{Global, JObject},
    signature::MethodSignature,
    strings::JNIStr,
};

use crate::error::AndroidBackendError;

/// One method of one object, checked to exist once and called many times.
pub(crate) struct BoundMethod {
    object: Global<JObject<'static>>,
    name: &'static JNIStr,
    signature: MethodSignature<'static, 'static>,
}

impl BoundMethod {
    /// Bind `name` with `signature` on `object`, which its class must declare.
    pub(crate) fn bind(
        env: &mut Env<'_>,
        object: Global<JObject<'static>>,
        name: &'static JNIStr,
        signature: MethodSignature<'static, 'static>,
    ) -> Result<Self, AndroidBackendError> {
        let class = env
            .get_object_class(&*object)
            .map_err(AndroidBackendError::jni("jni-object-class"))?;
        env.get_method_id(&class, name, &signature)
            .map_err(AndroidBackendError::jni("jni-method-id"))?;
        Ok(Self {
            object,
            name,
            signature,
        })
    }

    /// Call the method on the object it is bound to.
    pub(crate) fn call<'local>(
        &self,
        env: &mut Env<'local>,
        args: &[JValue],
    ) -> Result<JValueOwned<'local>, AndroidBackendError> {
        env.call_method(&*self.object, self.name, &self.signature, args)
            .map_err(AndroidBackendError::jni("jni-bound-method-call"))
    }
}
