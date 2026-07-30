# Prometheus and Grafana demo

This local demo runs Prometheus and Grafana with a preconfigured data source and the `Macmon Overview` dashboard.

Start macmon:

```sh
macmon serve
```

Then start the monitoring stack:

```sh
cd examples/grafana
docker compose up -d
```

| Service    | URL                   | Credentials         |
| ---------- | --------------------- | ------------------- |
| Prometheus | http://localhost:9091 | —                   |
| Grafana    | http://localhost:9000 | `macmon` / `macmon` |
