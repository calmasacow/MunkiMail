export const providerPresets = {
  google: { label: "Gmail / Google Workspace", imap_host: "imap.gmail.com", smtp_host: "smtp.gmail.com", smtp_port: 465, sent_folder: "[Gmail]/Sent Mail" },
  microsoft: { label: "Outlook / Microsoft 365", imap_host: "outlook.office365.com", smtp_host: "smtp.office365.com", smtp_port: 587, sent_folder: "Sent Items" },
  yahoo: { label: "Yahoo Mail", imap_host: "imap.mail.yahoo.com", smtp_host: "smtp.mail.yahoo.com", smtp_port: 465, sent_folder: "Sent" },
} as const;
