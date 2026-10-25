FROM python:3.12-slim-bookworm

ENV PYTHONUNBUFFERED=1 PYTHONDONTWRITEBYTECODE=1 QT_QPA_PLATFORM=offscreen
RUN apt-get update && apt-get install -y --no-install-recommends \
    libglib2.0-0 libgl1 libegl1 libxkbcommon0 libdbus-1-3 fontconfig fonts-noto-core fonts-noto-cjk fonts-noto-color-emoji \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY desktop/scripts/translation_preview/requirements-hosted.txt desktop/requirements.txt desktop/requirements-nodeps.txt /tmp/
RUN pip install --no-cache-dir -r /tmp/requirements-hosted.txt -r /tmp/requirements.txt \
    && pip install --no-cache-dir --no-deps -r /tmp/requirements-nodeps.txt
COPY desktop/telescope ./telescope
COPY desktop/update_guard.py ./
COPY desktop/scripts/translation_preview.py desktop/scripts/translation_project.py desktop/scripts/translation_review.py desktop/scripts/translation_account.py desktop/scripts/translation_hosted.py desktop/scripts/translation_scenes*.py desktop/scripts/text_fit.py ./scripts/
COPY desktop/scripts/translation_preview ./scripts/translation_preview
COPY desktop/translation-state ./translation-state
RUN python scripts/translation_preview.py --check
RUN useradd --system --create-home workbench && chown -R workbench:workbench /app
USER workbench
CMD exec gunicorn --bind "0.0.0.0:${PORT:-8080}" --workers 1 --threads 4 --timeout 620 --access-logfile - scripts.translation_hosted:application
