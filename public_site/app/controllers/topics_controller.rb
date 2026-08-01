# frozen_string_literal: true

class TopicsController < ApplicationController
  def index
    @topics = Topic.order(created_at: :desc)
  end

  def show
    @topic = Topic.find(params[:id])
  end
  def new
    @topic = Topic.new
  end

  def create
    @topic = Topic.new(topic_params)

    if @topic.save
      redirect_to topics_path, notice: "Topic created successfully."
    else
      render :new, status: :unprocessable_entity
    end
  end

  private
  def topic_params
    params.expect(topic: [:title, :body])
  end
end
