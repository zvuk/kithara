#include <jni.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

__attribute__((visibility("hidden"))) extern int main(int argc, char **argv);

__attribute__((weak)) void init_context(void *vm, void *context) {
    (void)vm;
    (void)context;
}

enum BootstrapError {
    LOG_PATH_FAILED = 110,
    LOG_OPEN_FAILED = 111,
    LOG_REDIRECT_FAILED = 112,
    DIRECTORY_STRING_FAILED = 113,
    DIRECTORY_CHANGE_FAILED = 114,
    ENVIRONMENT_LENGTH_INVALID = 115,
    ENVIRONMENT_STRINGS_FAILED = 116,
    ENVIRONMENT_SET_FAILED = 117,
    JAVA_VM_MISSING = 118,
    CONTEXT_REFERENCE_FAILED = 119,
    ARGUMENT_STRINGS_FAILED = 120,
};

static char **strings(JNIEnv *env, jobjectArray values, jsize count) {
    char **result = calloc((size_t)count + 1, sizeof(char *));
    if (!result) return NULL;
    for (jsize i = 0; i < count; ++i) {
        jstring item = (jstring)(*env)->GetObjectArrayElement(env, values, i);
        const char *value = (*env)->GetStringUTFChars(env, item, NULL);
        if (!value) goto failed;
        result[i] = strdup(value);
        (*env)->ReleaseStringUTFChars(env, item, value);
        (*env)->DeleteLocalRef(env, item);
        if (!result[i]) goto failed;
    }
    return result;
failed:
    for (jsize i = 0; i < count; ++i) free(result[i]);
    free(result);
    return NULL;
}

JNIEXPORT jint JNICALL Java_com_kithara_nativetest_NativeInstrumentation_runNative(
    JNIEnv *env, jclass cls, jobject context, jobjectArray arguments, jstring path,
    jstring directory, jobjectArray environment) {
    (void)cls;
    const char *log_path = (*env)->GetStringUTFChars(env, path, NULL);
    if (!log_path) return LOG_PATH_FAILED;
    int fd = open(log_path, O_WRONLY | O_CREAT | O_EXCL, 0600);
    (*env)->ReleaseStringUTFChars(env, path, log_path);
    if (fd < 0) return LOG_OPEN_FAILED;
    int redirected = dup2(fd, STDOUT_FILENO) >= 0 && dup2(fd, STDERR_FILENO) >= 0;
    close(fd);
    if (!redirected) return LOG_REDIRECT_FAILED;
    const char *cwd = (*env)->GetStringUTFChars(env, directory, NULL);
    if (!cwd) return DIRECTORY_STRING_FAILED;
    int changed = chdir(cwd);
    (*env)->ReleaseStringUTFChars(env, directory, cwd);
    if (changed != 0) return DIRECTORY_CHANGE_FAILED;
    jsize env_count = (*env)->GetArrayLength(env, environment);
    if (env_count % 2 != 0) return ENVIRONMENT_LENGTH_INVALID;
    char **vars = strings(env, environment, env_count);
    if (!vars) return ENVIRONMENT_STRINGS_FAILED;
    int assigned = 0;
    for (jsize i = 0; i < env_count; i += 2) {
        if (setenv(vars[i], vars[i + 1], 1) != 0) assigned = 1;
    }
    for (jsize i = 0; i < env_count; ++i) free(vars[i]);
    free(vars);
    if (assigned != 0) return ENVIRONMENT_SET_FAILED;
    JavaVM *vm = NULL;
    if ((*env)->GetJavaVM(env, &vm) != JNI_OK) return JAVA_VM_MISSING;
    jobject global_context = (*env)->NewGlobalRef(env, context);
    if (!global_context) return CONTEXT_REFERENCE_FAILED;
    init_context(vm, global_context);
    jsize argc = (*env)->GetArrayLength(env, arguments);
    char **argv = strings(env, arguments, argc);
    if (!argv) return ARGUMENT_STRINGS_FAILED;
    int code = main(argc, argv);
    fflush(stdout);
    fflush(stderr);
    for (jsize i = 0; i < argc; ++i) free(argv[i]);
    free(argv);
    return code;
}
