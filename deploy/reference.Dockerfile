ARG PYTHON_IMAGE
FROM ${PYTHON_IMAGE}
ARG UV_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends libsndfile1 \
    && rm -rf /var/lib/apt/lists/*
RUN python -m pip install --no-cache-dir "uv==${UV_VERSION}"
WORKDIR /workspace
COPY vendor/litellm /workspace/vendor/litellm
RUN uv export --frozen --no-dev --extra proxy --no-emit-workspace \
      --project vendor/litellm --output-file /tmp/requirements.txt --quiet \
    && uv pip sync --system --require-hashes /tmp/requirements.txt \
    && rm /tmp/requirements.txt
ENV PYTHONPATH=/workspace/vendor/litellm:/workspace/vendor/litellm/litellm-proxy-extras \
    LITELLM_RUST=false LITELLM_LOCAL_MODEL_COST_MAP=True PYTHONDONTWRITEBYTECODE=1
ENTRYPOINT ["python", "-m", "uvicorn", "litellm.proxy.proxy_server:app"]
