# Argus Agent (TypeScript). Runs via tsx (no build step needed).
FROM node:22-slim
WORKDIR /app
COPY agent/package.json ./
RUN npm install
COPY agent/ ./
EXPOSE 8787
CMD ["npm", "start"]
