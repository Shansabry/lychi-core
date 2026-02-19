<div class="dot-spinner">
	<span class="dot"></span>
	<span class="dot"></span>
	<span class="dot"></span>
</div>

<style>
	.dot-spinner {
		display: flex;
		justify-content: center;
		align-items: center;
		gap: 6px;
		padding: 32px 0;
	}

	.dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--fg-muted);
		opacity: 0.4;
		animation: dot-bounce 1s ease-in-out infinite;
	}

	.dot:nth-child(2) {
		animation-delay: 0.15s;
	}

	.dot:nth-child(3) {
		animation-delay: 0.3s;
	}

	@keyframes dot-bounce {
		0%, 60%, 100% {
			opacity: 0.4;
			transform: translateY(0);
		}
		30% {
			opacity: 1;
			transform: translateY(-4px);
		}
	}
</style>
