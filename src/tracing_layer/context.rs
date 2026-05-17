use tracing::Span;

/// Helper trait for setting user-provided GenAI context on spans
pub trait GenAISpanContext {
    /// Set agent identification information on the current span
    ///
    /// # Arguments
    /// * `agent_id` - Unique identifier for the agent
    /// * `agent_name` - Human-readable name for the agent
    ///
    /// # Example
    /// ```ignore
    /// use braintrust_sdk_rust::GenAISpanContext;
    ///
    /// let span = tracing::Span::current();
    /// span.set_agent_info("agent-123", "Customer Support Agent");
    /// ```
    fn set_agent_info(&self, agent_id: impl Into<String>, agent_name: impl Into<String>);

    /// Set workflow context information on the current span
    ///
    /// # Arguments
    /// * `workflow_id` - Unique identifier for the workflow
    /// * `step` - Current step number in the workflow
    ///
    /// # Example
    /// ```ignore
    /// use braintrust_sdk_rust::GenAISpanContext;
    ///
    /// let span = tracing::Span::current();
    /// span.set_workflow_info("workflow-abc", 3);
    /// ```
    fn set_workflow_info(&self, workflow_id: impl Into<String>, step: u32);
}

impl GenAISpanContext for Span {
    fn set_agent_info(&self, agent_id: impl Into<String>, agent_name: impl Into<String>) {
        self.record("gen_ai.agent.id", agent_id.into().as_str());
        self.record("gen_ai.agent.name", agent_name.into().as_str());
    }

    fn set_workflow_info(&self, workflow_id: impl Into<String>, step: u32) {
        self.record("gen_ai.workflow.id", workflow_id.into().as_str());
        self.record("gen_ai.workflow.step", step);
    }
}
