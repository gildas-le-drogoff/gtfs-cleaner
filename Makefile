build:
	cargo build --release --target x86_64-unknown-linux-musl --bin gtfs-cleaner
run:
	cargo run --release --target x86_64-unknown-linux-musl --bin gtfs-cleaner -- IDFM-gtfs.zip
fetch:
	curl -L https://eu.ftp.opendatasoft.com/stif/GTFS/IDFM-gtfs.zip -o IDFM-gtfs.zip
