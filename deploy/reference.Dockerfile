ARG PYTHON_IMAGE
FROM ${PYTHON_IMAGE}
ARG UV_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends libsndfile1 \
    && rm -rf /var/lib/apt/lists/*
RUN python -m pip install --no-cache-dir "uv==${UV_VERSION}"
WORKDIR /reference
# Build context must be the ignored reference directory, never the Rust checkout.
COPY pyproject.toml uv.lock README.md model_prices_and_context_window.json ./
COPY litellm litellm
COPY litellm_cli litellm_cli
COPY litellm-proxy-extras litellm-proxy-extras
RUN uv export --frozen --no-dev --extra proxy --no-emit-workspace \
      --project /reference --output-file /tmp/requirements.txt --quiet \
    && uv pip sync --system --require-hashes /tmp/requirements.txt \
    && rm /tmp/requirements.txt
ENV PYTHONPATH=/reference:/reference/litellm-proxy-extras \
    LITELLM_RUST=false LITELLM_LOCAL_MODEL_COST_MAP=True PYTHONDONTWRITEBYTECODE=1
ENTRYPOINT ["python", "-m", "uvicorn", "litellm.proxy.proxy_server:app"]
