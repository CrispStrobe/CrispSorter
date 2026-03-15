import { $state } from 'svelte/runes';

export interface ChatMessage {
    role: 'user' | 'assistant' | 'system';
    content: string;
    timestamp: number;
}

export class ChatStore {
    messages = $state<ChatMessage[]>([]);
    isThinking = $state(false);
    selectedDocs = $state<string[]>([]); // item IDs

    constructor() {}

    addMessage(role: 'user' | 'assistant' | 'system', content: string) {
        this.messages.push({
            role,
            content,
            timestamp: Date.now()
        });
    }

    clear() {
        this.messages = [];
    }
}

export const chatStore = new ChatStore();
