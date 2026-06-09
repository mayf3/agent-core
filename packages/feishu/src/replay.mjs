import { appendStateRecord, readStateRecords, writeStateRecords } from "../../core/src/index.mjs";

const fileName = "feishu_messages.jsonl";

export async function reserveFeishuMessage(stateDir, inbound) {
  const existing = await getFeishuMessage(stateDir, inbound.messageId);
  if (existing) {
    return { duplicate: true, record: existing };
  }
  const record = {
    messageId: inbound.messageId,
    chatId: inbound.chatId,
    status: "accepted",
    runId: null,
    replyMessageId: null,
    receivedAt: new Date().toISOString(),
    updatedAt: new Date().toISOString(),
  };
  await appendStateRecord(stateDir, fileName, record);
  return { duplicate: false, record };
}

export async function completeFeishuMessage(stateDir, messageId, patch = {}) {
  const records = await readStateRecords(stateDir, fileName);
  const index = records.findLastIndex((record) => record.messageId === messageId);
  if (index < 0) {
    return null;
  }
  const updated = {
    ...records[index],
    ...patch,
    status: patch.status || "replied",
    updatedAt: new Date().toISOString(),
  };
  records[index] = updated;
  await writeStateRecords(stateDir, fileName, records);
  return updated;
}

export async function getFeishuMessage(stateDir, messageId) {
  const records = await readStateRecords(stateDir, fileName);
  return records.findLast((record) => record.messageId === messageId) || null;
}
