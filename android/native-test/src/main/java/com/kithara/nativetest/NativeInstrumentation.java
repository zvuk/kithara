package com.kithara.nativetest;

import android.app.Activity;
import android.app.Instrumentation;
import android.content.Context;
import android.os.Bundle;
import org.json.JSONArray;
import org.json.JSONObject;

public final class NativeInstrumentation extends Instrumentation {
    private String request;

    private static native int runNative(
        Context context, String[] args, String logPath, String directory, String[] environment);

    @Override
    public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        request = arguments.getString("request");
        start();
    }

    @Override
    public void onStart() {
        Bundle result = new Bundle();
        try {
            JSONObject input = new JSONObject(request);
            Context context = getTargetContext().getApplicationContext();
            if (context == null) throw new IllegalStateException("Application Context is missing");
            String library = input.getString("library");
            if (!library.matches("kithara_test_[0-9a-f]+"))
                throw new IllegalArgumentException("Invalid test library name");
            System.loadLibrary(library);
            int code = runNative(context, strings(input.getJSONArray("args")),
                input.getString("log"), input.getString("directory"),
                strings(input.getJSONArray("environment")));
            result.putInt("rust_exit_code", code);
            finish(code == 0 ? Activity.RESULT_OK : Activity.RESULT_CANCELED, result);
        } catch (Throwable error) {
            result.putString("bootstrap_error", error.toString());
            finish(Activity.RESULT_CANCELED, result);
        }
    }

    private static String[] strings(JSONArray values) throws org.json.JSONException {
        String[] result = new String[values.length()];
        for (int i = 0; i < result.length; ++i) result[i] = values.getString(i);
        return result;
    }
}
