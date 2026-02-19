# Build stage
FROM rust:1.75 as builder

# Install trunk and wasm target
RUN cargo install trunk
RUN rustup target add wasm32-unknown-unknown

WORKDIR /app

# Copy project files
COPY Cargo.toml ./
COPY src ./src
COPY index.html ./
COPY styles.css ./
COPY Trunk.toml ./

# Build the application
RUN trunk build --release

# Production stage
FROM nginx:alpine

# Copy built files from builder
COPY --from=builder /app/dist /usr/share/nginx/html

# Copy nginx configuration
COPY nginx.conf /etc/nginx/conf.d/default.conf

EXPOSE 80

CMD ["nginx", "-g", "daemon off;"]

