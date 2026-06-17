import express from "express";
import { FailureContextSchema } from "./types.js";
import { decide } from "./decide.js";

const app = express();
app.use(express.json());

app.get("/health", (_req, res) => {
  res.json({ ok: true, service: "argus-agent" });
});

// The one boundary between Core and Agent (ADR 0001).
app.post("/decide", async (req, res) => {
  const parsed = FailureContextSchema.safeParse(req.body);
  if (!parsed.success) {
    res.status(400).json({ error: "invalid failure context", details: parsed.error.issues });
    return;
  }
  try {
    const decision = await decide(parsed.data);
    res.json(decision);
  } catch (err) {
    console.error("decide failed:", err);
    res.status(500).json({ error: "agent decision failed" });
  }
});

const port = Number(process.env.AGENT_PORT ?? 8787);
app.listen(port, () => console.log(`Argus agent listening on :${port}`));
