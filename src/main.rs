fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("delicium=info"))
        .init();
    if let Err(error) = delicium::app::run() {
        eprintln!("Delicium: {error}");
        std::process::exit(1);
    }
}
