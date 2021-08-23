/*
 * Copyright (C) 2021 The Android Open Source Project
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

package android.system.virtualmachine;

import static android.os.ParcelFileDescriptor.MODE_READ_ONLY;

import android.annotation.NonNull;
import android.content.Context;
import android.content.pm.PackageManager;
import android.content.pm.Signature; // This actually is certificate!
import android.os.ParcelFileDescriptor;
import android.os.PersistableBundle;
import android.system.virtualizationservice.VirtualMachineAppConfig;

import java.io.File;
import java.io.FileNotFoundException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

/**
 * Represents a configuration of a virtual machine. A configuration consists of hardware
 * configurations like the number of CPUs and the size of RAM, and software configurations like the
 * OS and application to run on the virtual machine.
 *
 * @hide
 */
public final class VirtualMachineConfig {
    // These defines the schema of the config file persisted on disk.
    private static final int VERSION = 1;
    private static final String KEY_VERSION = "version";
    private static final String KEY_CERTS = "certs";
    private static final String KEY_APKPATH = "apkPath";
    private static final String KEY_PAYLOADCONFIGPATH = "payloadConfigPath";
    private static final String KEY_DEBUGMODE = "debugMode";
    private static final String KEY_MEMORY_MIB = "memoryMib";

    // Paths to the APK file of this application.
    private final @NonNull String mApkPath;
    private final @NonNull Signature[] mCerts;
    private final boolean mDebugMode;
    /**
     * The amount of RAM to give the VM, in MiB. If this is 0 or negative the default will be used.
     */
    private final int mMemoryMib;

    /**
     * Path within the APK to the payload config file that defines software aspects of this config.
     */
    private final @NonNull String mPayloadConfigPath;

    // TODO(jiyong): add more items like # of cpu, size of ram, debuggability, etc.

    private VirtualMachineConfig(
            @NonNull String apkPath,
            @NonNull Signature[] certs,
            @NonNull String payloadConfigPath,
            boolean debugMode,
            int memoryMib) {
        mApkPath = apkPath;
        mCerts = certs;
        mPayloadConfigPath = payloadConfigPath;
        mDebugMode = debugMode;
        mMemoryMib = memoryMib;
    }

    /** Loads a config from a stream, for example a file. */
    /* package */ static @NonNull VirtualMachineConfig from(@NonNull InputStream input)
            throws IOException, VirtualMachineException {
        PersistableBundle b = PersistableBundle.readFromStream(input);
        final int version = b.getInt(KEY_VERSION);
        if (version > VERSION) {
            throw new VirtualMachineException("Version too high");
        }
        final String apkPath = b.getString(KEY_APKPATH);
        if (apkPath == null) {
            throw new VirtualMachineException("No apkPath");
        }
        final String[] certStrings = b.getStringArray(KEY_CERTS);
        if (certStrings == null || certStrings.length == 0) {
            throw new VirtualMachineException("No certs");
        }
        List<Signature> certList = new ArrayList<>();
        for (String s : certStrings) {
            certList.add(new Signature(s));
        }
        Signature[] certs = certList.toArray(new Signature[0]);
        final String payloadConfigPath = b.getString(KEY_PAYLOADCONFIGPATH);
        if (payloadConfigPath == null) {
            throw new VirtualMachineException("No payloadConfigPath");
        }
        final boolean debugMode = b.getBoolean(KEY_DEBUGMODE);
        final int memoryMib = b.getInt(KEY_MEMORY_MIB);
        return new VirtualMachineConfig(apkPath, certs, payloadConfigPath, debugMode, memoryMib);
    }

    /** Persists this config to a stream, for example a file. */
    /* package */ void serialize(@NonNull OutputStream output) throws IOException {
        PersistableBundle b = new PersistableBundle();
        b.putInt(KEY_VERSION, VERSION);
        b.putString(KEY_APKPATH, mApkPath);
        List<String> certList = new ArrayList<>();
        for (Signature cert : mCerts) {
            certList.add(cert.toCharsString());
        }
        String[] certs = certList.toArray(new String[0]);
        b.putStringArray(KEY_CERTS, certs);
        b.putString(KEY_PAYLOADCONFIGPATH, mPayloadConfigPath);
        b.putBoolean(KEY_DEBUGMODE, mDebugMode);
        if (mMemoryMib > 0) {
            b.putInt(KEY_MEMORY_MIB, mMemoryMib);
        }
        b.writeToStream(output);
    }

    /** Returns the path to the payload config within the owning application. */
    public @NonNull String getPayloadConfigPath() {
        return mPayloadConfigPath;
    }

    /**
     * Tests if this config is compatible with other config. Being compatible means that the configs
     * can be interchangeably used for the same virtual machine. Compatible changes includes the
     * number of CPUs and the size of the RAM, and change of the payload as long as the payload is
     * signed by the same signer. All other changes (e.g. using a payload from a different signer,
     * change of the debug mode, etc.) are considered as incompatible.
     */
    public boolean isCompatibleWith(@NonNull VirtualMachineConfig other) {
        if (!Arrays.equals(this.mCerts, other.mCerts)) {
            return false;
        }
        if (this.mDebugMode != other.mDebugMode) {
            return false;
        }
        return true;
    }

    /**
     * Converts this config object into a parcel. Used when creating a VM via the virtualization
     * service. Notice that the files are not passed as paths, but as file descriptors because the
     * service doesn't accept paths as it might not have permission to open app-owned files and that
     * could be abused to run a VM with software that the calling application doesn't own.
     */
    /* package */ VirtualMachineAppConfig toParcel() throws FileNotFoundException {
        VirtualMachineAppConfig parcel = new VirtualMachineAppConfig();
        parcel.apk = ParcelFileDescriptor.open(new File(mApkPath), MODE_READ_ONLY);
        parcel.configPath = mPayloadConfigPath;
        parcel.debug = mDebugMode;
        parcel.memoryMib = mMemoryMib;
        return parcel;
    }

    /** A builder used to create a {@link VirtualMachineConfig}. */
    public static class Builder {
        private Context mContext;
        private String mPayloadConfigPath;
        private boolean mDebugMode;
        private int mMemoryMib;
        // TODO(jiyong): add more items like # of cpu, size of ram, debuggability, etc.

        /** Creates a builder for the given context (APK), and the payload config file in APK. */
        public Builder(@NonNull Context context, @NonNull String payloadConfigPath) {
            mContext = context;
            mPayloadConfigPath = payloadConfigPath;
            mDebugMode = false;
        }

        /** Enables or disables the debug mode */
        public Builder debugMode(boolean enableOrDisable) {
            mDebugMode = enableOrDisable;
            return this;
        }

        /**
         * Sets the amount of RAM to give the VM. If this is zero or negative then the default will
         * be used.
         */
        public Builder memoryMib(int memoryMib) {
            mMemoryMib = memoryMib;
            return this;
        }

        /** Builds an immutable {@link VirtualMachineConfig} */
        public @NonNull VirtualMachineConfig build() {
            final String apkPath = mContext.getPackageCodePath();
            final String packageName = mContext.getPackageName();
            Signature[] certs;
            try {
                certs =
                        mContext.getPackageManager()
                                .getPackageInfo(
                                        packageName, PackageManager.GET_SIGNING_CERTIFICATES)
                                .signingInfo
                                .getSigningCertificateHistory();
            } catch (PackageManager.NameNotFoundException e) {
                // This cannot happen as `packageName` is from this app.
                throw new RuntimeException(e);
            }

            return new VirtualMachineConfig(
                    apkPath, certs, mPayloadConfigPath, mDebugMode, mMemoryMib);
        }
    }
}
