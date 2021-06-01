//! # PrometheusExporter
//!
//! `PrometheusExporter` implementation, expose metrics to
//! a [Prometheus](https://prometheus.io/) server.
use crate::current_system_time_since_epoch;
use crate::sensors::{Sensor, Topology};
use crate::exporters::Exporter;
use chrono::Utc;
use clap::{Arg, ArgMatches};
use std::{collections::HashMap, net::{IpAddr, SocketAddr}, str::FromStr, sync::Arc, time::Duration};
use super::utils::get_hostname;
use std::convert::Infallible;
use hyper::{Body, Request, Response, Server};
use hyper::service::{make_service_fn, service_fn};
use hyper::server::conn::AddrStream;

/// Default ipv4/ipv6 address to expose the service is any
const DEFAULT_IP_ADDRESS: &str = "::";

/// Exporter that exposes metrics to an HTTP endpoint
/// matching the Prometheus.io metrics format.
pub struct PrometheusExporter {
    /// Sensor instance that is used to generate the Topology and
    /// thus get power consumption metrics.
    sensor: Box<dyn Sensor>,
}

impl PrometheusExporter {
    /// Instantiates PrometheusExporter and returns the instance.
    pub fn new(sensor: Box<dyn Sensor>) -> PrometheusExporter {
        PrometheusExporter { sensor }
    }
}

impl Exporter for PrometheusExporter {
    /// Entry point ot the PrometheusExporter.
    ///
    /// Runs HTTP server and metrics exposure through the runner function.
    fn run(&mut self, parameters: ArgMatches) {
        info!(
            "{}: Starting Prometheus exporter",
            Utc::now().format("%Y-%m-%dT%H:%M:%S")
        );
        println!("Press CTRL-C to stop scaphandre");

        runner(
            (*self.sensor.get_topology()).unwrap(),
            parameters.value_of("address").unwrap().to_string(),
            parameters.value_of("port").unwrap().to_string(),
            parameters.value_of("suffix").unwrap().to_string(),
            parameters.is_present("qemu"),
            parameters.is_present("containers"),
            get_hostname(),
        );
    }
    /// Returns options understood by the exporter.
    fn get_options() -> Vec<clap::Arg<'static, 'static>> {
        let mut options = Vec::new();
        let arg = Arg::with_name("address")
            .default_value(DEFAULT_IP_ADDRESS)
            .help("ipv6 or ipv4 address to expose the service to")
            .long("address")
            .short("a")
            .required(false)
            .takes_value(true);
        options.push(arg);

        let arg = Arg::with_name("port")
            .default_value("8080")
            .help("TCP port number to expose the service")
            .long("port")
            .short("p")
            .required(false)
            .takes_value(true);
        options.push(arg);

        let arg = Arg::with_name("suffix")
            .default_value("metrics")
            .help("url suffix to access metrics")
            .long("suffix")
            .short("s")
            .required(false)
            .takes_value(true);
        options.push(arg);

        let arg = Arg::with_name("qemu")
            .help("Instruct that scaphandre is running on an hypervisor")
            .long("qemu")
            .short("q")
            .required(false)
            .takes_value(false);
        options.push(arg);

        let arg = Arg::with_name("containers")
            .help("Monitor and apply labels for processes running as containers")
            .long("containers")
            .required(false)
            .takes_value(false);
        options.push(arg);

        options
    }
}

/// Contains a mutex holding a Topology object.
/// Used to pass the topology data from one http worker to another.
struct PowerMetrics {
    topology: Topology,
    last_request: Duration,
    qemu: bool,
    containers: bool,
    hostname: String,
}

#[tokio::main]
async fn runner(
    topology: Topology, address: String, port: String, suffix: String, qemu: bool, containers: bool, hostname: String,
){
    if let Ok(addr) = address.parse::<IpAddr>() {
        if let Ok(port) = port.parse::<u16>() {
            let socket_addr = SocketAddr::new(addr, port);
            let context = Arc::new(PowerMetrics {
                topology: topology.clone(),
                last_request: Duration::new(0, 0),
                qemu,
                containers,
                hostname: hostname.clone(),
            });
            let make_svc = make_service_fn(move |_| {
                async {
                    Ok::<_, Infallible>(
                            service_fn( move |req| {
                                show_metrics(req)
                            }
                        )
                    )
                }
            });
            let server = Server::bind(&socket_addr);
            let res = server.serve(make_svc);

            if let Err(e) = res.await {
                error!("server error: {}", e);
            }
        } else {
            panic!("{} is not a valid TCP port number", port);
        }
    } else {
        panic!("{} is not a valid ip address", address);
    }
}

//#[actix_web::main]
///// Main function running the HTTP server.
//async fn runner(
//    topology: Topology,
//    address: String,
//    port: String,
//    suffix: String,
//    qemu: bool,
//    containers: bool,
//    hostname: String,
//) -> std::io::Result<()> {
//    if let Err(error) = address.parse::<IpAddr>() {
//        panic!("{} is not a valid ip address: {}", address, error);
//    }
//    if let Err(error) = port.parse::<u64>() {
//        panic!("Not a valid TCP port numer: {}", error);
//    }
//
//    HttpServer::new(move || {
//        App::new()
//            .data(PowerMetrics {
//                topology: Mutex::new(topology.clone()),
//                last_request: Mutex::new(Duration::new(0, 0)),
//                qemu,
//                containers,
//                hostname: hostname.clone(),
//            })
//            .service(web::resource(&suffix).route(web::get().to(show_metrics)))
//            .default_service(web::route().to(landing_page))
//    })
//    .workers(1)
//    .bind(format!("{}:{}", address, port))?
//    .run()
//    .await
//}
//
/// Returns a well formatted Prometheus metric string.
fn format_metric(key: &str, value: &str, labels: Option<&HashMap<String, String>>) -> String {
    let mut result = key.to_string();
    if let Some(labels) = labels {
        result.push('{');
        for (k, v) in labels.iter() {
            result.push_str(&format!("{}=\"{}\",", k, v));
        }
        result.remove(result.len() - 1);
        result.push('}');
    }
    result.push_str(&format!(" {}\n", value));
    result
}

/// Adds lines related to a metric in the body (String) of response.
fn push_metric(
    mut body: String,
    help: String,
    metric_type: String,
    metric_name: String,
    metric_line: String,
) -> String {
    body.push_str(&format!("# HELP {} {}", metric_name, help));
    body.push_str(&format!("\n# TYPE {} {}\n", metric_name, metric_type));
    body.push_str(&metric_line);
    body
}

//#[derive(Clone, Copy)]
//struct Router {
//
//}

//impl Router {
    /// Handles requests and returns data formated for Prometheus.
    async fn show_metrics(req: Request<Body>) -> Result<Response<Body>, Infallible> {
        warn!("{}", req.uri());
        let mut body = String::new();
        if req.uri().path() == "/metrics" {
            body.push_str("Here come tha metriczzz !!!");
        } else {
            body.push_str("go to /metrics !!");
        }
        Ok(Response::new(body.into()))
    }

//}
//async fn show_metrics(context: Arc<PowerMetrics>, req: Request<Body>) -> Result<Response<Body>, Infallible> {
//    Ok(Response::new("Coucou toi !".into()))
//}
//async fn show_metrics(data: web::Data<PowerMetrics>) -> impl Responder {
//    let now = current_system_time_since_epoch();
//    let mut last_request = data.last_request.lock().unwrap();
//
//    if now - (*last_request) > Duration::from_secs(5) {
//        {
//            info!(
//                "{}: Refresh topology",
//                Utc::now().format("%Y-%m-%dT%H:%M:%S")
//            );
//            let mut topology = data.topology.lock().unwrap();
//            (*topology)
//                .proc_tracker
//                .clean_terminated_process_records_vectors();
//            (*topology).refresh();
//        }
//    }
//
//    *last_request = now;
//    let topo = data.topology.lock().unwrap();
//    let mut metric_generator = MetricGenerator::new(&*topo, &data.hostname);
//
//    info!("{}: Refresh data", Utc::now().format("%Y-%m-%dT%H:%M:%S"));
//    let mut body = String::from(""); // initialize empty body
//
//    metric_generator.gen_all_metrics(data.qemu, data.containers);
//
//    // Send all data
//    for msg in metric_generator.get_metrics() {
//        let mut attributes: Option<&HashMap<String, String>> = None;
//        if !msg.attributes.is_empty() {
//            attributes = Some(&msg.attributes);
//        }
//
//        let value = match msg.metric_value {
//            // MetricValueType::IntSigned(value) => event.set_metric_sint64(value),
//            // MetricValueType::Float(value) => event.set_metric_f(value),
//            MetricValueType::FloatDouble(value) => value.to_string(),
//            MetricValueType::IntUnsigned(value) => value.to_string(),
//            MetricValueType::Text(ref value) => value.to_string(),
//        };
//        body = push_metric(
//            body,
//            msg.description.clone(),
//            msg.metric_type.clone(),
//            msg.name.clone(),
//            format_metric(&msg.name, &value, attributes),
//        );
//    }
//
//    HttpResponse::Ok()
//        //.set_header("X-TEST", "value")
//        .body(body)
//}
//
///// Handles requests that are not asking for /metrics and returns the appropriate path in the body of the response.
//async fn landing_page() -> impl Responder {
//    let body = String::from(
//        "<a href=\"https://github.com/hubblo-org/scaphandre/\">Scaphandre's</a> prometheus exporter here. Metrics available on <a href=\"/metrics\">/metrics</a>"
//    );
//    HttpResponse::Ok()
//        //.set_header("X-TEST", "value")
//        .body(body)
//}
//
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
