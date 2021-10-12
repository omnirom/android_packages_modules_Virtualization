// Copyright 2021, The Android Open Source Project
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Payload disk image

use android_system_virtualizationservice::aidl::android::system::virtualizationservice::{
    DiskImage::DiskImage, Partition::Partition, VirtualMachineAppConfig::VirtualMachineAppConfig,
    VirtualMachineRawConfig::VirtualMachineRawConfig,
};
use android_system_virtualizationservice::binder::ParcelFileDescriptor;
use anyhow::{anyhow, Context, Result};
use binder::{wait_for_interface, Strong};
use log::{error, info};
use microdroid_metadata::{ApexPayload, ApkPayload, Metadata};
use microdroid_payload_config::{ApexConfig, VmPayloadConfig};
use once_cell::sync::OnceCell;
use packagemanager_aidl::aidl::android::content::pm::IPackageManagerNative::IPackageManagerNative;
use serde::Deserialize;
use serde_xml_rs::from_reader;
use std::env;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use vmconfig::open_parcel_file;

/// The list of APEXes which microdroid requires.
// TODO(b/192200378) move this to microdroid.json?
const MICRODROID_REQUIRED_APEXES: [&str; 2] = ["com.android.adbd", "com.android.os.statsd"];

const APEX_INFO_LIST_PATH: &str = "/apex/apex-info-list.xml";

const PACKAGE_MANAGER_NATIVE_SERVICE: &str = "package_native";

/// Represents the list of APEXes
#[derive(Debug, Deserialize)]
struct ApexInfoList {
    #[serde(rename = "apex-info")]
    list: Vec<ApexInfo>,
}

#[derive(Debug, Deserialize)]
struct ApexInfo {
    #[serde(rename = "moduleName")]
    name: String,
    #[serde(rename = "modulePath")]
    path: PathBuf,
}

impl ApexInfoList {
    /// Loads ApexInfoList
    fn load() -> Result<&'static ApexInfoList> {
        static INSTANCE: OnceCell<ApexInfoList> = OnceCell::new();
        INSTANCE.get_or_try_init(|| {
            let apex_info_list = File::open(APEX_INFO_LIST_PATH)
                .context(format!("Failed to open {}", APEX_INFO_LIST_PATH))?;
            let apex_info_list: ApexInfoList = from_reader(apex_info_list)
                .context(format!("Failed to parse {}", APEX_INFO_LIST_PATH))?;
            Ok(apex_info_list)
        })
    }

    fn get_path_for(&self, apex_name: &str) -> Result<PathBuf> {
        Ok(self
            .list
            .iter()
            .find(|apex| apex.name == apex_name)
            .ok_or_else(|| anyhow!("{} not found.", apex_name))?
            .path
            .clone())
    }
}

struct PackageManager {
    service: Strong<dyn IPackageManagerNative>,
    // TODO(b/199146189) use IPackageManagerNative
    apex_info_list: &'static ApexInfoList,
}

impl PackageManager {
    fn new() -> Result<Self> {
        let service = wait_for_interface(PACKAGE_MANAGER_NATIVE_SERVICE)
            .context("Failed to find PackageManager")?;
        let apex_info_list = ApexInfoList::load()?;
        Ok(Self { service, apex_info_list })
    }

    fn get_apex_path(&self, name: &str, prefer_staged: bool) -> Result<PathBuf> {
        if prefer_staged {
            let apex_info = self.service.getStagedApexInfo(name)?;
            if let Some(apex_info) = apex_info {
                info!("prefer_staged: use {} for {}", apex_info.diskImagePath, name);
                return Ok(PathBuf::from(apex_info.diskImagePath));
            }
        }
        self.apex_info_list.get_path_for(name)
    }
}

fn make_metadata_file(
    config_path: &str,
    apex_names: &[String],
    temporary_directory: &Path,
) -> Result<ParcelFileDescriptor> {
    let metadata_path = temporary_directory.join("metadata");
    let metadata = Metadata {
        version: 1,
        apexes: apex_names
            .iter()
            .enumerate()
            .map(|(i, apex_name)| ApexPayload {
                name: apex_name.clone(),
                partition_name: format!("microdroid-apex-{}", i),
                ..Default::default()
            })
            .collect(),
        apk: Some(ApkPayload {
            name: "apk".to_owned(),
            payload_partition_name: "microdroid-apk".to_owned(),
            idsig_partition_name: "microdroid-apk-idsig".to_owned(),
            ..Default::default()
        })
        .into(),
        payload_config_path: format!("/mnt/apk/{}", config_path),
        ..Default::default()
    };

    // Write metadata to file.
    let mut metadata_file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&metadata_path)
        .with_context(|| format!("Failed to open metadata file {:?}", metadata_path))?;
    microdroid_metadata::write_metadata(&metadata, &mut metadata_file)?;

    // Re-open the metadata file as read-only.
    open_parcel_file(&metadata_path, false)
}

/// Creates a DiskImage with partitions:
///   metadata: metadata
///   microdroid-apex-0: apex 0
///   microdroid-apex-1: apex 1
///   ..
///   microdroid-apk: apk
///   microdroid-apk-idsig: idsig
fn make_payload_disk(
    apk_file: File,
    idsig_file: File,
    config_path: &str,
    apexes: &[String],
    prefer_staged: bool,
    temporary_directory: &Path,
) -> Result<DiskImage> {
    let metadata_file = make_metadata_file(config_path, apexes, temporary_directory)?;
    // put metadata at the first partition
    let mut partitions = vec![Partition {
        label: "payload-metadata".to_owned(),
        image: Some(metadata_file),
        writable: false,
    }];

    let pm = PackageManager::new()?;
    for (i, apex) in apexes.iter().enumerate() {
        let apex_path = pm.get_apex_path(apex, prefer_staged)?;
        let apex_file = open_parcel_file(&apex_path, false)?;
        partitions.push(Partition {
            label: format!("microdroid-apex-{}", i),
            image: Some(apex_file),
            writable: false,
        });
    }
    partitions.push(Partition {
        label: "microdroid-apk".to_owned(),
        image: Some(ParcelFileDescriptor::new(apk_file)),
        writable: false,
    });
    partitions.push(Partition {
        label: "microdroid-apk-idsig".to_owned(),
        image: Some(ParcelFileDescriptor::new(idsig_file)),
        writable: false,
    });

    Ok(DiskImage { image: None, partitions, writable: false })
}

fn find_apex_names_in_classpath_env(classpath_env_var: &str) -> Vec<String> {
    let val = env::var(classpath_env_var).unwrap_or_else(|e| {
        error!("Reading {} failed: {}", classpath_env_var, e);
        String::from("")
    });
    val.split(':')
        .filter_map(|path| {
            Path::new(path)
                .strip_prefix("/apex/")
                .map(|stripped| {
                    let first = stripped.iter().next().unwrap();
                    first.to_str().unwrap().to_string()
                })
                .ok()
        })
        .collect()
}

// Collect APEX names from config
fn collect_apex_names(apexes: &[ApexConfig]) -> Vec<String> {
    // Process pseudo names like "{BOOTCLASSPATH}".
    // For now we have following pseudo APEX names:
    // - {BOOTCLASSPATH}: represents APEXes contributing "BOOTCLASSPATH" environment variable
    // - {DEX2OATBOOTCLASSPATH}: represents APEXes contributing "DEX2OATBOOTCLASSPATH" environment variable
    // - {SYSTEMSERVERCLASSPATH}: represents APEXes contributing "SYSTEMSERVERCLASSPATH" environment variable
    let mut apex_names: Vec<String> = apexes
        .iter()
        .flat_map(|apex| match apex.name.as_str() {
            "{BOOTCLASSPATH}" => find_apex_names_in_classpath_env("BOOTCLASSPATH"),
            "{DEX2OATBOOTCLASSPATH}" => find_apex_names_in_classpath_env("DEX2OATBOOTCLASSPATH"),
            "{SYSTEMSERVERCLASSPATH}" => find_apex_names_in_classpath_env("SYSTEMSERVERCLASSPATH"),
            _ => vec![apex.name.clone()],
        })
        .collect();
    // Add required APEXes
    apex_names.extend(MICRODROID_REQUIRED_APEXES.iter().map(|name| name.to_string()));
    apex_names.sort();
    apex_names.dedup();
    apex_names
}

pub fn add_microdroid_images(
    config: &VirtualMachineAppConfig,
    temporary_directory: &Path,
    apk_file: File,
    idsig_file: File,
    instance_file: File,
    vm_payload_config: &VmPayloadConfig,
    vm_config: &mut VirtualMachineRawConfig,
) -> Result<()> {
    // collect APEX names from config
    let apexes = collect_apex_names(&vm_payload_config.apexes);
    info!("Microdroid payload APEXes: {:?}", apexes);
    vm_config.disks.push(make_payload_disk(
        apk_file,
        idsig_file,
        &config.configPath,
        &apexes,
        vm_payload_config.prefer_staged,
        temporary_directory,
    )?);

    if config.debug {
        vm_config.disks[1].partitions.push(Partition {
            label: "bootconfig".to_owned(),
            image: Some(open_parcel_file(
                Path::new("/apex/com.android.virt/etc/microdroid_bootconfig.debug"),
                false,
            )?),
            writable: false,
        });
    }

    // instance image is at the second partition in the second disk.
    vm_config.disks[1].partitions.push(Partition {
        label: "vm-instance".to_owned(),
        image: Some(ParcelFileDescriptor::new(instance_file)),
        writable: true,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_find_apex_names_in_classpath_env() {
        let key = "TEST_BOOTCLASSPATH";
        let classpath = "/apex/com.android.foo/javalib/foo.jar:/system/framework/framework.jar:/apex/com.android.bar/javalib/bar.jar";
        env::set_var(key, classpath);
        assert_eq!(
            find_apex_names_in_classpath_env(key),
            vec!["com.android.foo".to_owned(), "com.android.bar".to_owned()]
        );
    }
}
