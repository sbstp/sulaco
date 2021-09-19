FROM python:3.9-alpine3.14

COPY tests/zombie/* /app/
COPY target/x86_64-unknown-linux-musl/debug/sulaco /bin/sulaco

WORKDIR /app

CMD [ "/bin/sulaco", "sulaco.yaml" ]
