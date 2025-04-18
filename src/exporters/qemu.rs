use crate::exporters::Exporter;
use crate::sensors::Topology;
use crate::sensors::{utils::ProcessRecord, Sensor};
use std::path::{Path, PathBuf};
use std::{fs, io, thread, time};

/// An Exporter that extracts power consumption data of running
/// Qemu/KVM virtual machines on the host and store those data
/// as folders and files that are supposed to be mounted on the
/// guest/virtual machines. This allow users of the virtual machines
/// to collect and deal with their power consumption metrics, the same way
/// they would do it if they managed bare metal machines.
pub struct QemuExporter {
    // We don't need a MetricGenerator for this exporter, because it "justs"
    // puts the metrics in files in the same way as the powercap kernel module.
    topology: Topology,
}

impl Exporter for QemuExporter {
    /// Runs [iterate()] in a loop.
    fn run(&mut self) {
        info!("Starting qemu exporter");
        let path = "/var/lib/libvirt/scaphandre";
        let cleaner_step = 120;
        let mut timer = time::Duration::from_secs(cleaner_step);
        loop {
            self.iterate(String::from(path));
            let step = time::Duration::from_secs(5);
            thread::sleep(step);
            if timer - step > time::Duration::from_millis(0) {
                timer -= step;
            } else {
                self.topology
                    .proc_tracker
                    .clean_terminated_process_records_vectors();
                timer = time::Duration::from_secs(cleaner_step);
            }
        }
    }

    fn kind(&self) -> &str {
        "qemu"
    }
}

impl QemuExporter {
    /// Instantiates and returns a new QemuExporter
    pub fn new(sensor: &dyn Sensor) -> QemuExporter {
        let topology = sensor
            .get_topology()
            .expect("sensor topology should be available");
        QemuExporter { topology }
    }

    /// Processes the metrics of `self.topology` and exposes them at the given `path`.
    pub fn iterate(&mut self, path: String) {
        trace!("path: {}", path);
        self.topology.refresh();
        let Some(uw_dynamic_rec) = self.topology.get_records_power_diff_microwatts_dynamic() else {
            return;
        };
        let uw_dynamic = uw_dynamic_rec.value.parse::<f64>().unwrap();
        let Some(t_diff) = self.topology.get_records_time_diff() else {
            return;
        };
        let uj_dynamic = uw_dynamic * t_diff;

        let processes = self.topology.proc_tracker.get_alive_processes();
        let qemu_processes = QemuExporter::filter_qemu_vm_processes(&processes);
        for qp in qemu_processes {
            if qp.len() > 2 {
                let last = qp.first().unwrap();
                let vm_name = QemuExporter::get_vm_name_from_cmdline(
                    &last.process.cmdline(&self.topology.proc_tracker).unwrap(),
                );

                let exported_path = format!("{path}/{vm_name}");
                if let Some(proc_utilization) = self
                    .topology
                    .get_process_attribution_factor(last.process.pid)
                {
                    let uj_to_add = proc_utilization * uj_dynamic;
                    match QemuExporter::add_or_create(&PathBuf::from(exported_path.clone()), uj_to_add as u64) {
                        Ok(result) => {
                            trace!("{:?}", result);
                            debug!("Updated {}", exported_path);
                        }
                        Err(err) => {
                            error!(
                                "Could'nt edit {}. Please check file permissions : {}",
                                exported_path, err
                            );
                        }
                    }
                }
            }
        }
    }

    /// Parses a cmdline String (as contained in procs::Process instances) and returns
    /// the name of the qemu virtual machine if this process is a qemu/kvm guest process
    fn get_vm_name_from_cmdline(cmdline: &[String]) -> String {
        for elmt in cmdline {
            if elmt.starts_with("guest=") {
                let mut splitted = elmt.split('=');
                splitted.next();
                return String::from(splitted.next().unwrap().split(',').next().unwrap());
            }
        }
        // Extract Proxmox vmids too
        for pair in cmdline.windows(2) {
            if pair[0] == "-id" {
                return pair[1].clone();
            }
        }
        String::from("") // TODO return Option<String> None instead, and stop at line 76 (it won't work with {path}//intel-rapl)
    }

    /// Creates an export mimicking the powercap RAPL directory structure at the specified location
    /// Either creates an energy_uj file (as the ones managed by powercap kernel module)
    /// in 'path' and adds 'uj_value' to its numerical content, or simply performs the
    /// addition if the file exists.
    /// Also creates a fake "core" subdomain with the same uj value to improve compatibility.
    fn add_or_create(path: &Path, uj_value: u64) -> io::Result<()> {
        if !path.exists() {
            match fs::create_dir_all(path) {
                Ok(_) => info!("Created {} folder.", path.to_string_lossy()),
                Err(error) => panic!("Couldn't create {}. Got: {}", path.to_string_lossy(), error),
            }
        }
        let domain_package = path.join("intel-rapl:0");
        if !domain_package.exists() {
            match fs::create_dir_all(&domain_package) {
                Ok(_) => info!("Created {} folder.", domain_package.clone().to_string_lossy()),
                Err(error) => panic!("Couldn't create {}. Got: {}", domain_package.to_string_lossy(), error),
            }
        }
        fs::write(domain_package.join("name"), "package-0")?;

        let domain_core = path.join("intel-rapl:0/intel-rapl:0:0");
        if !domain_core.exists() {
            match fs::create_dir_all(&domain_core) {
                Ok(_) => info!("Created {} folder.", domain_core.clone().to_string_lossy()),
                Err(error) => panic!("Couldn't create {}. Got: {}", domain_core.to_string_lossy(), error),
            }
        }
        fs::write(domain_core.join("name"), "core")?;

        let mut content = 0;

        if let Ok(file) = fs::read_to_string(domain_package.join("energy_uj")) {
            content = file.parse::<u64>().unwrap();
            content += uj_value;
        }
        fs::write(domain_package.join("energy_uj"), content.to_string())?;
        fs::write(domain_core.join("energy_uj"), content.to_string())
    }

    /// Filters 'processes' to match processes that look like qemu/kvm guest processes.
    /// Returns what was found.
    fn filter_qemu_vm_processes(processes: &[&Vec<ProcessRecord>]) -> Vec<Vec<ProcessRecord>> {
        let mut qemu_processes: Vec<Vec<ProcessRecord>> = vec![];
        trace!("Got {} processes to filter.", processes.len());
        for vecp in processes.iter() {
            if !vecp.is_empty() {
                if let Some(pr) = vecp.first() {
                    if let Some(res) = pr
                        .process
                        .cmdline
                        .iter()
                        .find(|x| x.contains("qemu-system") || x.contains("/usr/bin/kvm"))
                    {
                        debug!("Found a process with {}", res);
                        let mut tmp: Vec<ProcessRecord> = vec![];
                        for p in vecp.iter() {
                            tmp.push(p.clone());
                        }
                        qemu_processes.push(tmp);
                    }
                }
            }
        }
        qemu_processes
    }
}

//  Copyright 2020 The scaphandre authors.
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
